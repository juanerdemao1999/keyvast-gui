use std::{fmt, path::PathBuf};

#[cfg(windows)]
mod imp {
    use std::{
        ffi::{CString, c_char, c_int, c_long, c_ulong, c_void},
        path::{Path, PathBuf},
        sync::Arc,
    };

    use libloading::Library;

    use super::FrontPanelError;

    type Handle = *mut c_void;
    type OkErrorCode = c_int;
    type Bool = c_int;

    const OK_NO_ERROR: OkErrorCode = 0;

    pub struct FrontPanelLibrary {
        api: Arc<FrontPanelApi>,
    }

    struct FrontPanelApi {
        _library: Library,
        construct: unsafe extern "system" fn() -> Handle,
        destruct: unsafe extern "system" fn(Handle),
        get_device_count: unsafe extern "system" fn(Handle) -> c_int,
        get_device_list_serial: unsafe extern "system" fn(Handle, c_int, *mut c_char),
        get_device_list_model: unsafe extern "system" fn(Handle, c_int) -> c_int,
        open_by_serial: unsafe extern "system" fn(Handle, *const c_char) -> OkErrorCode,
        is_open: unsafe extern "system" fn(Handle) -> Bool,
        configure_fpga: unsafe extern "system" fn(Handle, *const c_char) -> OkErrorCode,
        is_frontpanel_enabled: unsafe extern "system" fn(Handle) -> Bool,
        set_wire_in_value:
            unsafe extern "system" fn(Handle, c_int, c_ulong, c_ulong) -> OkErrorCode,
        update_wire_ins: unsafe extern "system" fn(Handle),
        update_wire_outs: unsafe extern "system" fn(Handle),
        get_wire_out_value: unsafe extern "system" fn(Handle, c_int) -> c_ulong,
        activate_trigger_in: unsafe extern "system" fn(Handle, c_int, c_int) -> OkErrorCode,
        read_from_block_pipe_out:
            unsafe extern "system" fn(Handle, c_int, c_int, c_long, *mut u8) -> c_long,
    }

    impl FrontPanelLibrary {
        pub fn load(path: Option<PathBuf>) -> Result<Self, FrontPanelError> {
            let path = path.unwrap_or_else(default_frontpanel_dll_path);
            log::info!("loading FrontPanel DLL: {}", path.display());
            // SAFETY: Library::new loads the DLL into the process address space.
            // The path is a valid filesystem path. The Library is stored in
            // Arc<FrontPanelApi>::_library, keeping the DLL loaded for the
            // lifetime of all derived function pointers.
            let library = unsafe {
                Library::new(&path).map_err(|source| {
                    log::error!("FrontPanel DLL load FAILED: {source}");
                    FrontPanelError::DllLoad {
                        path: path.clone(),
                        message: source.to_string(),
                    }
                })?
            };

            // SAFETY: Each symbol name is a NUL-terminated byte literal matching
            // the Opal Kelly FrontPanel C API. The Library remains alive via
            // Arc<FrontPanelApi>::_library for as long as any function pointer
            // is reachable. Symbol types match the vendor header signatures.
            let api = unsafe {
                FrontPanelApi {
                    construct: symbol(&library, b"okFrontPanel_Construct\0")?,
                    destruct: symbol(&library, b"okFrontPanel_Destruct\0")?,
                    get_device_count: symbol(&library, b"okFrontPanel_GetDeviceCount\0")?,
                    get_device_list_serial: symbol(
                        &library,
                        b"okFrontPanel_GetDeviceListSerial\0",
                    )?,
                    get_device_list_model: symbol(&library, b"okFrontPanel_GetDeviceListModel\0")?,
                    open_by_serial: symbol(&library, b"okFrontPanel_OpenBySerial\0")?,
                    is_open: symbol(&library, b"okFrontPanel_IsOpen\0")?,
                    configure_fpga: symbol(&library, b"okFrontPanel_ConfigureFPGA\0")?,
                    is_frontpanel_enabled: symbol(&library, b"okFrontPanel_IsFrontPanelEnabled\0")?,
                    set_wire_in_value: symbol(&library, b"okFrontPanel_SetWireInValue\0")?,
                    update_wire_ins: symbol(&library, b"okFrontPanel_UpdateWireIns\0")?,
                    update_wire_outs: symbol(&library, b"okFrontPanel_UpdateWireOuts\0")?,
                    get_wire_out_value: symbol(&library, b"okFrontPanel_GetWireOutValue\0")?,
                    activate_trigger_in: symbol(&library, b"okFrontPanel_ActivateTriggerIn\0")?,
                    read_from_block_pipe_out: symbol(
                        &library,
                        b"okFrontPanel_ReadFromBlockPipeOut\0",
                    )?,
                    _library: library,
                }
            };

            log::info!("FrontPanel DLL loaded OK");
            Ok(Self { api: Arc::new(api) })
        }

        pub fn open_device(
            &self,
            serial: Option<&str>,
        ) -> Result<FrontPanelDevice, FrontPanelError> {
            // SAFETY: construct() allocates a new FrontPanel handle; the
            // returned pointer is checked for null immediately below.
            let handle = unsafe { (self.api.construct)() };
            if handle.is_null() {
                return Err(FrontPanelError::ConstructFailed);
            }

            let device = FrontPanelDevice {
                api: Arc::clone(&self.api),
                handle,
            };

            let serial = match serial {
                Some(serial) => serial.to_string(),
                None => device.first_serial()?,
            };
            let serial_display = serial.clone();
            let serial = CString::new(serial)
                .map_err(|_| FrontPanelError::InvalidCString { field: "serial" })?;
            log::info!("opening device by serial: {serial_display}");
            device.check_error(
                "okFrontPanel_OpenBySerial",
                // SAFETY: handle is non-null (checked above), serial is a
                // NUL-terminated CString kept alive for the duration of the call.
                unsafe { (device.api.open_by_serial)(device.handle, serial.as_ptr()) },
            )?;

            // SAFETY: handle is non-null and exclusively owned by `device`.
            let is_open = unsafe { (device.api.is_open)(device.handle) != 0 };
            if !is_open {
                log::error!("device reported NOT open after OpenBySerial");
                return Err(FrontPanelError::DeviceNotOpen);
            }

            log::info!("device opened OK (serial={serial_display})");
            Ok(device)
        }
    }

    pub struct FrontPanelDevice {
        api: Arc<FrontPanelApi>,
        handle: Handle,
    }

    impl FrontPanelDevice {
        pub fn configure_fpga(&self, bitfile: &Path) -> Result<(), FrontPanelError> {
            let size = std::fs::metadata(bitfile).map(|m| m.len()).unwrap_or(0);
            log::info!("ConfigureFPGA: {} ({size} bytes)", bitfile.display());
            if size == 0 {
                log::warn!(
                    "bitfile is missing or empty at this path — \
                     ConfigureFPGA will program nothing"
                );
            }
            let bitfile_c = path_to_cstring(bitfile)
                .map_err(|_| FrontPanelError::InvalidCString { field: "bitfile" })?;
            self.check_error(
                "okFrontPanel_ConfigureFPGA",
                // SAFETY: handle is non-null and exclusively owned; bitfile_c is a
                // NUL-terminated CString kept alive for the duration of the call.
                unsafe { (self.api.configure_fpga)(self.handle, bitfile_c.as_ptr()) },
            )?;

            // SAFETY: handle is non-null and exclusively owned.
            let enabled = unsafe { (self.api.is_frontpanel_enabled)(self.handle) } != 0;
            log::info!("ConfigureFPGA returned OK; FrontPanel enabled = {enabled}");
            if !enabled {
                return Err(FrontPanelError::FrontPanelNotEnabled);
            }

            Ok(())
        }

        pub fn set_wire_in_value(
            &self,
            endpoint: i32,
            value: u32,
            mask: u32,
        ) -> Result<(), FrontPanelError> {
            self.check_error(
                "okFrontPanel_SetWireInValue",
                // SAFETY: handle is non-null and exclusively owned; scalar
                // arguments are passed by value.
                unsafe {
                    (self.api.set_wire_in_value)(
                        self.handle,
                        endpoint,
                        value as c_ulong,
                        mask as c_ulong,
                    )
                },
            )
        }

        pub fn update_wire_ins(&self) {
            // SAFETY: handle is non-null and exclusively owned.
            unsafe { (self.api.update_wire_ins)(self.handle) };
        }

        pub fn update_wire_outs(&self) {
            // SAFETY: handle is non-null and exclusively owned.
            unsafe { (self.api.update_wire_outs)(self.handle) };
        }

        pub fn get_wire_out_value(&self, endpoint: i32) -> u32 {
            // SAFETY: handle is non-null and exclusively owned.
            unsafe { (self.api.get_wire_out_value)(self.handle, endpoint) }
        }

        pub fn activate_trigger_in(&self, endpoint: i32, bit: i32) -> Result<(), FrontPanelError> {
            self.check_error(
                "okFrontPanel_ActivateTriggerIn",
                // SAFETY: handle is non-null and exclusively owned.
                unsafe { (self.api.activate_trigger_in)(self.handle, endpoint, bit) },
            )
        }

        pub fn read_from_block_pipe_out(
            &self,
            endpoint: i32,
            block_size: usize,
            buffer: &mut [u8],
        ) -> Result<usize, FrontPanelError> {
            // SAFETY: handle is non-null and exclusively owned. buffer is a
            // valid mutable slice; buffer.len() fits in c_long for all practical
            // block sizes (max ~100 KB). The API writes at most `len` bytes into
            // the buffer and returns the actual byte count (or negative on error).
            debug_assert!(buffer.len() <= c_long::MAX as usize);
            let read = unsafe {
                (self.api.read_from_block_pipe_out)(
                    self.handle,
                    endpoint,
                    block_size as c_int,
                    buffer.len() as c_long,
                    buffer.as_mut_ptr(),
                )
            };

            if read < 0 {
                return Err(FrontPanelError::TransferFailed {
                    function: "okFrontPanel_ReadFromBlockPipeOut",
                    code: read,
                });
            }

            Ok(read as usize)
        }

        fn first_serial(&self) -> Result<String, FrontPanelError> {
            // SAFETY: handle is non-null and exclusively owned.
            let count = unsafe { (self.api.get_device_count)(self.handle) };
            log::info!("FrontPanel device count: {count}");
            if count <= 0 {
                log::info!(
                    "no Opal Kelly device found — check the USB3 cable, the \
                     FrontPanel/USB driver, and that no other program (e.g. Open Ephys) \
                     still holds the board open"
                );
                return Err(FrontPanelError::NoDevices);
            }

            for index in 0..count {
                let mut buffer = [0_i8; 64];
                // SAFETY: handle is non-null; buffer is a stack-allocated
                // [i8; 64] valid for the duration of the call. The API writes a
                // NUL-terminated ASCII serial string into the buffer.
                unsafe {
                    (self.api.get_device_list_serial)(self.handle, index, buffer.as_mut_ptr())
                };
                let serial =
                    c_buffer_to_string(&buffer).unwrap_or_else(|_| "<invalid>".to_string());
                // SAFETY: handle is non-null and exclusively owned.
                let model = unsafe { (self.api.get_device_list_model)(self.handle, index) };
                log::info!(
                    "  device[{index}]: serial={serial} model={model} ({})",
                    model_name(model)
                );
            }

            let mut buffer = [0_i8; 64];
            // SAFETY: same as above — handle is non-null, buffer is valid.
            unsafe { (self.api.get_device_list_serial)(self.handle, 0, buffer.as_mut_ptr()) };
            c_buffer_to_string(&buffer)
        }

        fn check_error(
            &self,
            function: &'static str,
            code: OkErrorCode,
        ) -> Result<(), FrontPanelError> {
            if code == OK_NO_ERROR {
                return Ok(());
            }

            Err(FrontPanelError::Api { function, code })
        }
    }

    impl Drop for FrontPanelDevice {
        fn drop(&mut self) {
            // SAFETY: handle is non-null (checked at construction) and exclusively
            // owned — no aliasing is possible since FrontPanelDevice is not Clone
            // or Copy. The Library (held by Arc<FrontPanelApi>::_library) outlives
            // this call because Arc::clone was taken at construction time.
            unsafe { (self.api.destruct)(self.handle) };
        }
    }

    /// # Safety
    /// The caller must ensure `name` is a NUL-terminated byte literal matching
    /// a symbol exported by the loaded `library`, and that `T` matches the
    /// symbol's actual calling convention and signature.
    unsafe fn symbol<T: Copy>(
        library: &Library,
        name: &'static [u8],
    ) -> Result<T, FrontPanelError> {
        // SAFETY: name is a NUL-terminated byte literal; the library is alive
        // and will remain alive via Arc<FrontPanelApi>::_library.
        let symbol =
            unsafe { library.get::<T>(name) }.map_err(|source| FrontPanelError::MissingSymbol {
                name: String::from_utf8_lossy(&name[..name.len().saturating_sub(1)]).to_string(),
                message: source.to_string(),
            })?;
        Ok(*symbol)
    }

    fn c_buffer_to_string(buffer: &[i8]) -> Result<String, FrontPanelError> {
        let bytes = buffer
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte as u8)
            .collect::<Vec<_>>();

        String::from_utf8(bytes).map_err(|_| FrontPanelError::InvalidUtf8 { field: "serial" })
    }

    fn path_to_cstring(path: &Path) -> Result<CString, std::ffi::NulError> {
        CString::new(path.to_string_lossy().as_bytes())
    }

    fn model_name(code: c_int) -> &'static str {
        match code {
            0 => "unknown/none",
            43 => "XEM7310-A75",
            44 => "XEM7310-A200",
            _ => "other",
        }
    }

    pub fn default_frontpanel_dll_path() -> PathBuf {
        let dll_name = "okFrontPanel.dll";
        let relative_vendor = std::path::Path::new("third_party")
            .join("opalkelly")
            .join("windows-x64")
            .join(dll_name);

        // 1. Next to the executable (deployed builds).
        if let Ok(exe) = std::env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            let candidate = exe_dir.join(dll_name);
            if candidate.exists() {
                return candidate;
            }
            // Also check vendor sub-path relative to exe
            let candidate = exe_dir.join(&relative_vendor);
            if candidate.exists() {
                return candidate;
            }
        }

        // 2. Current working directory.
        if let Ok(cwd) = std::env::current_dir() {
            let candidate = cwd.join(dll_name);
            if candidate.exists() {
                return candidate;
            }
            let candidate = cwd.join(&relative_vendor);
            if candidate.exists() {
                return candidate;
            }
        }

        // 3. Fallback: compile-time source tree (development only).
        #[cfg(debug_assertions)]
        {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let candidate = manifest.join("..").join("..").join(&relative_vendor);
            if let Ok(path) = candidate.canonicalize() {
                return path;
            }
        }

        // Last resort: bare name, let the OS DLL search find it.
        PathBuf::from(dll_name)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    use super::FrontPanelError;

    pub struct FrontPanelLibrary;
    pub struct FrontPanelDevice;

    impl FrontPanelLibrary {
        pub fn load(_path: Option<PathBuf>) -> Result<Self, FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }

        pub fn open_device(
            &self,
            _serial: Option<&str>,
        ) -> Result<FrontPanelDevice, FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }
    }

    impl FrontPanelDevice {
        pub fn configure_fpga(&self, _bitfile: &Path) -> Result<(), FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }

        pub fn set_wire_in_value(
            &self,
            _endpoint: i32,
            _value: u32,
            _mask: u32,
        ) -> Result<(), FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }

        pub fn update_wire_ins(&self) {}

        pub fn update_wire_outs(&self) {}

        pub fn get_wire_out_value(&self, _endpoint: i32) -> u32 {
            0
        }

        pub fn activate_trigger_in(
            &self,
            _endpoint: i32,
            _bit: i32,
        ) -> Result<(), FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }

        pub fn read_from_block_pipe_out(
            &self,
            _endpoint: i32,
            _block_size: usize,
            _buffer: &mut [u8],
        ) -> Result<usize, FrontPanelError> {
            Err(FrontPanelError::UnsupportedPlatform)
        }
    }

    pub fn default_frontpanel_dll_path() -> PathBuf {
        PathBuf::from("okFrontPanel.dll")
    }
}

pub use imp::{FrontPanelDevice, FrontPanelLibrary, default_frontpanel_dll_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontPanelError {
    UnsupportedPlatform,
    DllLoad { path: PathBuf, message: String },
    MissingSymbol { name: String, message: String },
    ConstructFailed,
    NoDevices,
    DeviceNotOpen,
    InvalidCString { field: &'static str },
    InvalidUtf8 { field: &'static str },
    FrontPanelNotEnabled,
    Api { function: &'static str, code: i32 },
    TransferFailed { function: &'static str, code: i32 },
}

impl fmt::Display for FrontPanelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => write!(
                formatter,
                "Opal Kelly FrontPanel hardware access is currently available on Windows only"
            ),
            Self::DllLoad { path, message } => {
                write!(
                    formatter,
                    "failed to load FrontPanel DLL {}: {message}",
                    path.display()
                )
            }
            Self::MissingSymbol { name, message } => {
                write!(
                    formatter,
                    "FrontPanel DLL is missing symbol {name}: {message}"
                )
            }
            Self::ConstructFailed => write!(formatter, "failed to construct FrontPanel device"),
            Self::NoDevices => write!(formatter, "no Opal Kelly FrontPanel devices were found"),
            Self::DeviceNotOpen => write!(formatter, "Opal Kelly device did not open"),
            Self::InvalidCString { field } => {
                write!(formatter, "{field} contains an interior NUL byte")
            }
            Self::InvalidUtf8 { field } => write!(formatter, "{field} is not valid UTF-8"),
            Self::FrontPanelNotEnabled => write!(
                formatter,
                "configured FPGA bitfile does not expose FrontPanel endpoints"
            ),
            Self::Api { function, code } => {
                write!(
                    formatter,
                    "{function} returned FrontPanel error code {code}"
                )
            }
            Self::TransferFailed { function, code } => {
                write!(formatter, "{function} returned transfer status {code}")
            }
        }
    }
}

impl std::error::Error for FrontPanelError {}
