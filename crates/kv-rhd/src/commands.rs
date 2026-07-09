use std::fmt;

use crate::protocol::{DEFAULT_RHD_SAMPLE_RATE, SAMPLES_PER_USB_BLOCK};

pub const RHD_COMMAND_LIST_LEN: usize = 128;
pub const RHD_ADC_CALIBRATION_SAMPLES: usize = SAMPLES_PER_USB_BLOCK;
/// Maximum command sequence length for on-FPGA RAM banks.
pub const MAX_COMMAND_LENGTH: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rhd2000CommandType {
    Convert,
    Calibrate,
    ClearCalibration,
    RegisterWrite,
    RegisterRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxCommandSlot {
    AuxCmd1,
    AuxCmd2,
    AuxCmd3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardPort {
    PortA,
    PortB,
    PortC,
    PortD,
    PortE,
    PortF,
    PortG,
    PortH,
}

impl BoardPort {
    pub fn all() -> [Self; 8] {
        [
            Self::PortA,
            Self::PortB,
            Self::PortC,
            Self::PortD,
            Self::PortE,
            Self::PortF,
            Self::PortG,
            Self::PortH,
        ]
    }

    pub fn bit_shift(self) -> u32 {
        match self {
            Self::PortA => 0,
            Self::PortB => 4,
            Self::PortC => 8,
            Self::PortD => 12,
            Self::PortE => 16,
            Self::PortF => 20,
            Self::PortG => 24,
            Self::PortH => 28,
        }
    }
}

pub fn create_rhd2000_command(
    command_type: Rhd2000CommandType,
    arg1: Option<u8>,
    arg2: Option<u8>,
) -> Result<u16, RhdCommandError> {
    match command_type {
        Rhd2000CommandType::Calibrate if arg1.is_none() && arg2.is_none() => Ok(0x5500),
        Rhd2000CommandType::ClearCalibration if arg1.is_none() && arg2.is_none() => Ok(0x6a00),
        Rhd2000CommandType::Convert if arg2.is_none() => {
            let channel = arg1.ok_or(RhdCommandError::MissingArgument {
                command_type,
                argument: "channel",
            })?;
            if channel > 63 {
                return Err(RhdCommandError::ArgumentOutOfRange {
                    command_type,
                    argument: "channel",
                    value: channel as u16,
                    max: 63,
                });
            }
            Ok((channel as u16) << 8)
        }
        Rhd2000CommandType::RegisterRead if arg2.is_none() => {
            let register = arg1.ok_or(RhdCommandError::MissingArgument {
                command_type,
                argument: "register",
            })?;
            if register > 63 {
                return Err(RhdCommandError::ArgumentOutOfRange {
                    command_type,
                    argument: "register",
                    value: register as u16,
                    max: 63,
                });
            }
            Ok(0xc000 + ((register as u16) << 8))
        }
        Rhd2000CommandType::RegisterWrite => {
            let register = arg1.ok_or(RhdCommandError::MissingArgument {
                command_type,
                argument: "register",
            })?;
            if register > 63 {
                return Err(RhdCommandError::ArgumentOutOfRange {
                    command_type,
                    argument: "register",
                    value: register as u16,
                    max: 63,
                });
            }
            let value = arg2.ok_or(RhdCommandError::MissingArgument {
                command_type,
                argument: "value",
            })?;
            Ok(0x8000 + ((register as u16) << 8) + value as u16)
        }
        _ => Err(RhdCommandError::InvalidArgumentShape { command_type }),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rhd2000Registers {
    sample_rate: f64,
    adc_reference_bw: u8,
    amp_fast_settle: u8,
    amp_vref_enable: u8,
    adc_comparator_bias: u8,
    adc_comparator_select: u8,
    vdd_sense_enable: u8,
    adc_buffer_bias: u8,
    mux_bias: u8,
    mux_load: u8,
    temp_s1: u8,
    temp_s2: u8,
    temp_en: u8,
    dig_out_hi_z: u8,
    dig_out: u8,
    weak_miso: u8,
    twos_comp: u8,
    abs_mode: u8,
    dsp_en: u8,
    dsp_cutoff_freq: u8,
    zcheck_dac_power: u8,
    zcheck_load: u8,
    zcheck_scale: u8,
    zcheck_conn_all: u8,
    zcheck_sel_pol: u8,
    zcheck_en: u8,
    zcheck_select: u8,
    off_chip_rh1: u8,
    off_chip_rh2: u8,
    off_chip_rl: u8,
    adc_aux1_en: u8,
    adc_aux2_en: u8,
    adc_aux3_en: u8,
    rh1_dac1: u8,
    rh1_dac2: u8,
    rh2_dac1: u8,
    rh2_dac2: u8,
    rl_dac1: u8,
    rl_dac2: u8,
    rl_dac3: u8,
    amp_power: [u8; 64],
}

impl Rhd2000Registers {
    pub fn new(sample_rate: f64) -> Self {
        let mut registers = Self {
            sample_rate,
            adc_reference_bw: 3,
            amp_fast_settle: 0,
            amp_vref_enable: 1,
            adc_comparator_bias: 3,
            adc_comparator_select: 2,
            vdd_sense_enable: 1,
            adc_buffer_bias: 0,
            mux_bias: 0,
            mux_load: 0,
            temp_s1: 0,
            temp_s2: 0,
            temp_en: 0,
            dig_out_hi_z: 1,
            dig_out: 0,
            weak_miso: 1,
            twos_comp: 0,
            abs_mode: 0,
            dsp_en: 0,
            dsp_cutoff_freq: 0,
            zcheck_dac_power: 1,
            zcheck_load: 0,
            zcheck_scale: 0,
            zcheck_conn_all: 0,
            zcheck_sel_pol: 0,
            zcheck_en: 0,
            zcheck_select: 0,
            off_chip_rh1: 0,
            off_chip_rh2: 0,
            off_chip_rl: 0,
            adc_aux1_en: 1,
            adc_aux2_en: 1,
            adc_aux3_en: 1,
            rh1_dac1: 0,
            rh1_dac2: 0,
            rh2_dac1: 0,
            rh2_dac2: 0,
            rl_dac1: 0,
            rl_dac2: 0,
            rl_dac3: 0,
            amp_power: [1; 64],
        };
        registers.define_sample_rate(sample_rate);
        registers.set_dsp_cutoff_freq(1.0);
        // Match the Open Ephys RHD plugin reference recording (Record Node settings.xml):
        //   HighCut=7500 (nominal) -> ~7604 Hz actual, LowCut=0.0955 Hz, DSPOffset=0.
        // The on-chip DSP high-pass (offset removal) is left OFF at configure time
        // (enable_dsp(false) in rhythm_board), and the analog lower bandwidth is dropped
        // to ~0.0955 Hz, so DC offset + <~4 Hz LFP are preserved exactly as OE records
        // them. (set_dsp_cutoff_freq above is inert while DSP is disabled, mirroring OE
        // which still carries a DSPCutoffFreq value with DSPOffset=0.)
        registers.set_upper_bandwidth(7_500.0);
        registers.set_lower_bandwidth(0.0955);
        registers
    }

    pub fn open_ephys_default() -> Self {
        Self::new(DEFAULT_RHD_SAMPLE_RATE)
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn set_fast_settle(&mut self, enabled: bool) {
        self.amp_fast_settle = u8::from(enabled);
    }

    pub fn set_dig_out_low(&mut self) {
        self.dig_out = 0;
        self.dig_out_hi_z = 0;
    }

    pub fn enable_aux_inputs(&mut self, enabled: bool) {
        let value = u8::from(enabled);
        self.adc_aux1_en = value;
        self.adc_aux2_en = value;
        self.adc_aux3_en = value;
    }

    pub fn enable_dsp(&mut self, enabled: bool) {
        self.dsp_en = u8::from(enabled);
    }

    pub fn enable_zcheck(&mut self, enabled: bool) {
        self.zcheck_en = u8::from(enabled);
    }

    pub fn set_zcheck_scale(&mut self, scale: ZcheckScale) {
        self.zcheck_scale = match scale {
            ZcheckScale::Cs100fF => 0x00,
            ZcheckScale::Cs1pF => 0x01,
            ZcheckScale::Cs10pF => 0x03,
        };
    }

    pub fn set_zcheck_polarity(&mut self, negative: bool) {
        self.zcheck_sel_pol = u8::from(negative);
    }

    pub fn set_zcheck_channel(&mut self, channel: u8) {
        self.zcheck_select = channel.min(63);
    }

    pub fn register_value(&self, register: u8) -> Result<u8, RhdCommandError> {
        // Registers are packed bit-fields: combine the (non-overlapping) fields
        // with bitwise OR rather than `+` so a stray out-of-range field can
        // never overflow the u8 during construction.
        let value = match register {
            0 => {
                (self.adc_reference_bw << 6)
                    | (self.amp_fast_settle << 5)
                    | (self.amp_vref_enable << 4)
                    | (self.adc_comparator_bias << 2)
                    | self.adc_comparator_select
            }
            1 => (self.vdd_sense_enable << 6) | self.adc_buffer_bias,
            2 => self.mux_bias,
            3 => {
                (self.mux_load << 5)
                    | (self.temp_s2 << 4)
                    | (self.temp_s1 << 3)
                    | (self.temp_en << 2)
                    | (self.dig_out_hi_z << 1)
                    | self.dig_out
            }
            4 => {
                (self.weak_miso << 7)
                    | (self.twos_comp << 6)
                    | (self.abs_mode << 5)
                    | (self.dsp_en << 4)
                    | self.dsp_cutoff_freq
            }
            5 => {
                (self.zcheck_dac_power << 6)
                    | (self.zcheck_load << 5)
                    | (self.zcheck_scale << 3)
                    | (self.zcheck_conn_all << 2)
                    | (self.zcheck_sel_pol << 1)
                    | self.zcheck_en
            }
            6 => 128,
            7 => self.zcheck_select,
            8 => (self.off_chip_rh1 << 7) | self.rh1_dac1,
            9 => (self.adc_aux1_en << 7) | self.rh1_dac2,
            10 => (self.off_chip_rh2 << 7) | self.rh2_dac1,
            11 => (self.adc_aux2_en << 7) | self.rh2_dac2,
            12 => (self.off_chip_rl << 7) | self.rl_dac1,
            13 => (self.adc_aux3_en << 7) | (self.rl_dac3 << 6) | self.rl_dac2,
            14..=21 => self.amp_power_register(register),
            _ => {
                return Err(RhdCommandError::ArgumentOutOfRange {
                    command_type: Rhd2000CommandType::RegisterRead,
                    argument: "register",
                    value: register as u16,
                    max: 21,
                });
            }
        };

        Ok(value)
    }

    pub fn create_command_list_register_config(
        &self,
        calibrate: bool,
    ) -> Result<Vec<u16>, RhdCommandError> {
        let mut commands = Vec::with_capacity(RHD_COMMAND_LIST_LEN);

        commands.push(reg_read(63)?);
        commands.push(reg_read(63)?);
        commands.push(reg_write(0, self.register_value(0)?)?);
        commands.push(reg_write(1, self.register_value(1)?)?);
        commands.push(reg_write(2, self.register_value(2)?)?);
        // Skip Register 3 (MUX Load, Temperature Sensor, Auxiliary Digital
        // Output) — it is controlled by AuxCmd1 (dig out) and AuxCmd2 (temp
        // sensor).  Writing it here would overwrite the temperature sensor
        // control bits set by AuxCmd2, breaking temp readings.
        commands.push(reg_write(4, self.register_value(4)?)?);
        commands.push(reg_write(5, self.register_value(5)?)?);
        // Skip Register 6 (Impedance Check DAC) — controlled by a dedicated
        // impedance command stream.
        commands.push(reg_write(7, self.register_value(7)?)?);
        for register in 8..=17 {
            commands.push(reg_write(register, self.register_value(register)?)?);
        }

        for register in [
            63, 62, 61, 60, 59, 48, 49, 50, 51, 52, 53, 54, 55, 40, 41, 42, 43, 44,
        ] {
            commands.push(reg_read(register)?);
        }
        for register in 0..=17 {
            commands.push(reg_read(register)?);
        }

        if calibrate {
            commands.push(create_rhd2000_command(
                Rhd2000CommandType::Calibrate,
                None,
                None,
            )?);
        } else {
            commands.push(reg_read(63)?);
        }

        for register in 18..=21 {
            commands.push(reg_write(register, self.register_value(register)?)?);
        }
        commands.push(reg_read(63)?);
        while commands.len() < RHD_COMMAND_LIST_LEN {
            commands.push(reg_read(63)?);
        }

        Ok(commands)
    }

    pub fn create_command_list_temp_sensor(&mut self) -> Result<Vec<u16>, RhdCommandError> {
        let mut commands = Vec::with_capacity(RHD_COMMAND_LIST_LEN);
        self.temp_en = 1;

        self.push_aux_triplet(&mut commands)?;
        self.temp_s1 = self.temp_en;
        self.temp_s2 = 0;
        commands.push(reg_write(3, self.register_value(3)?)?);

        self.push_aux_triplet(&mut commands)?;
        self.temp_s1 = self.temp_en;
        self.temp_s2 = self.temp_en;
        commands.push(reg_write(3, self.register_value(3)?)?);

        self.push_aux_triplet(&mut commands)?;
        commands.push(convert(49)?);

        self.push_aux_triplet(&mut commands)?;
        self.temp_s1 = 0;
        self.temp_s2 = self.temp_en;
        commands.push(reg_write(3, self.register_value(3)?)?);

        self.push_aux_triplet(&mut commands)?;
        commands.push(convert(49)?);

        self.push_aux_triplet(&mut commands)?;
        self.temp_s1 = 0;
        self.temp_s2 = 0;
        commands.push(reg_write(3, self.register_value(3)?)?);

        self.push_aux_triplet(&mut commands)?;
        commands.push(convert(48)?);

        for _ in 0..25 {
            self.push_aux_triplet(&mut commands)?;
            commands.push(reg_read(63)?);
        }

        Ok(commands)
    }

    pub fn create_command_list_update_dig_out(&mut self) -> Result<Vec<u16>, RhdCommandError> {
        let mut commands = Vec::with_capacity(RHD_COMMAND_LIST_LEN);
        self.temp_en = 1;

        for _ in 0..3 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }
        self.temp_s1 = self.temp_en;
        self.temp_s2 = 0;
        commands.push(reg_write(3, self.register_value(3)?)?);

        for _ in 0..3 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }
        self.temp_s1 = self.temp_en;
        self.temp_s2 = self.temp_en;
        commands.push(reg_write(3, self.register_value(3)?)?);

        for _ in 0..4 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }

        for _ in 0..3 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }
        self.temp_s1 = 0;
        self.temp_s2 = self.temp_en;
        commands.push(reg_write(3, self.register_value(3)?)?);

        for _ in 0..4 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }

        for _ in 0..3 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }
        self.temp_s1 = 0;
        self.temp_s2 = 0;
        commands.push(reg_write(3, self.register_value(3)?)?);

        for _ in 0..4 {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }

        while commands.len() < RHD_COMMAND_LIST_LEN {
            commands.push(reg_write(3, self.register_value(3)?)?);
        }

        Ok(commands)
    }

    fn define_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.mux_load = 0;

        if sample_rate < 3334.0 {
            self.mux_bias = 40;
            self.adc_buffer_bias = 32;
        } else if sample_rate < 4001.0 {
            self.mux_bias = 40;
            self.adc_buffer_bias = 16;
        } else if sample_rate < 5001.0 {
            self.mux_bias = 40;
            self.adc_buffer_bias = 8;
        } else if sample_rate < 6251.0 {
            self.mux_bias = 32;
            self.adc_buffer_bias = 8;
        } else if sample_rate < 8001.0 {
            self.mux_bias = 26;
            self.adc_buffer_bias = 8;
        } else if sample_rate < 10001.0 {
            self.mux_bias = 18;
            self.adc_buffer_bias = 4;
        } else if sample_rate < 12501.0 {
            self.mux_bias = 16;
            self.adc_buffer_bias = 3;
        } else if sample_rate < 15001.0 {
            self.mux_bias = 7;
            self.adc_buffer_bias = 3;
        } else {
            self.mux_bias = 4;
            self.adc_buffer_bias = 2;
        }
    }

    fn set_dsp_cutoff_freq(&mut self, new_dsp_cutoff_freq: f64) {
        let pi = 2.0 * f64::acos(0.0);
        let log_new = f64::log10(new_dsp_cutoff_freq);
        let mut f_cutoff = [0.0_f64; 16];
        let mut log_f_cutoff = [0.0_f64; 16];

        for n in 1..16 {
            let x = f64::powi(2.0, n as i32);
            f_cutoff[n] = self.sample_rate * f64::ln(x / (x - 1.0)) / (2.0 * pi);
            log_f_cutoff[n] = f64::log10(f_cutoff[n]);
        }

        self.dsp_cutoff_freq = if new_dsp_cutoff_freq > f_cutoff[1] {
            1
        } else if new_dsp_cutoff_freq < f_cutoff[15] {
            15
        } else {
            let mut best = 1_u8;
            let mut min_log_diff = f64::MAX;
            for (n, &log_f) in log_f_cutoff.iter().enumerate().skip(1) {
                let diff = f64::abs(log_new - log_f);
                if diff < min_log_diff {
                    min_log_diff = diff;
                    best = n as u8;
                }
            }
            best
        };
    }

    fn set_upper_bandwidth(&mut self, upper_bandwidth: f64) {
        let upper_bandwidth = upper_bandwidth.min(30_000.0);
        self.rh1_dac1 = 0;
        self.rh1_dac2 = 0;
        let mut rh1_actual = 2200.0;
        let rh1_target = rh1_from_upper_bandwidth(upper_bandwidth);
        for _ in 0..31 {
            if rh1_actual < rh1_target - (29_400.0 - 600.0 / 2.0) {
                rh1_actual += 29_400.0;
                self.rh1_dac2 += 1;
            }
        }
        for _ in 0..63 {
            if rh1_actual < rh1_target - (600.0 / 2.0) {
                rh1_actual += 600.0;
                self.rh1_dac1 += 1;
            }
        }

        self.rh2_dac1 = 0;
        self.rh2_dac2 = 0;
        let mut rh2_actual = 8700.0;
        let rh2_target = rh2_from_upper_bandwidth(upper_bandwidth);
        for _ in 0..31 {
            if rh2_actual < rh2_target - (38_400.0 - 763.0 / 2.0) {
                rh2_actual += 38_400.0;
                self.rh2_dac2 += 1;
            }
        }
        for _ in 0..63 {
            if rh2_actual < rh2_target - (763.0 / 2.0) {
                rh2_actual += 763.0;
                self.rh2_dac1 += 1;
            }
        }
    }

    fn set_lower_bandwidth(&mut self, lower_bandwidth: f64) {
        let lower_bandwidth = lower_bandwidth.min(1500.0);
        self.rl_dac1 = 0;
        self.rl_dac2 = 0;
        self.rl_dac3 = 0;
        let mut rl_actual = 3500.0;
        let rl_target = rl_from_lower_bandwidth(lower_bandwidth);

        if lower_bandwidth < 0.15 {
            rl_actual += 3_000_000.0;
            self.rl_dac3 += 1;
        }
        for _ in 0..63 {
            if rl_actual < rl_target - (12_700.0 - 175.0 / 2.0) {
                rl_actual += 12_700.0;
                self.rl_dac2 += 1;
            }
        }
        for _ in 0..127 {
            if rl_actual < rl_target - (175.0 / 2.0) {
                rl_actual += 175.0;
                self.rl_dac1 += 1;
            }
        }
    }

    fn amp_power_register(&self, register: u8) -> u8 {
        let start = (register as usize - 14) * 8;
        let mut value = 0_u8;
        for bit in 0..8 {
            value += self.amp_power[start + bit] << bit;
        }
        value
    }

    fn push_aux_triplet(&self, commands: &mut Vec<u16>) -> Result<(), RhdCommandError> {
        commands.push(convert(32)?);
        commands.push(convert(33)?);
        commands.push(convert(34)?);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RhdCommandError {
    MissingArgument {
        command_type: Rhd2000CommandType,
        argument: &'static str,
    },
    ArgumentOutOfRange {
        command_type: Rhd2000CommandType,
        argument: &'static str,
        value: u16,
        max: u16,
    },
    InvalidArgumentShape {
        command_type: Rhd2000CommandType,
    },
}

impl fmt::Display for RhdCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArgument {
                command_type,
                argument,
            } => write!(formatter, "{command_type:?} is missing {argument} argument"),
            Self::ArgumentOutOfRange {
                command_type,
                argument,
                value,
                max,
            } => write!(
                formatter,
                "{command_type:?} {argument} value {value} exceeds maximum {max}"
            ),
            Self::InvalidArgumentShape { command_type } => {
                write!(
                    formatter,
                    "{command_type:?} was called with invalid arguments"
                )
            }
        }
    }
}

impl std::error::Error for RhdCommandError {}

fn convert(channel: u8) -> Result<u16, RhdCommandError> {
    create_rhd2000_command(Rhd2000CommandType::Convert, Some(channel), None)
}

fn reg_read(register: u8) -> Result<u16, RhdCommandError> {
    create_rhd2000_command(Rhd2000CommandType::RegisterRead, Some(register), None)
}

fn reg_write(register: u8, value: u8) -> Result<u16, RhdCommandError> {
    create_rhd2000_command(
        Rhd2000CommandType::RegisterWrite,
        Some(register),
        Some(value),
    )
}

/// Series capacitor scale for impedance testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZcheckScale {
    /// 0.1 pF — for high impedance electrodes (> ~1 MΩ).
    Cs100fF,
    /// 1.0 pF — for medium impedance electrodes (~100 kΩ–1 MΩ).
    Cs1pF,
    /// 10.0 pF — for low impedance electrodes (< ~100 kΩ).
    Cs10pF,
}

impl ZcheckScale {
    /// Capacitance in farads.
    pub fn capacitance_farads(self) -> f64 {
        match self {
            Self::Cs100fF => 0.1e-12,
            Self::Cs1pF => 1.0e-12,
            Self::Cs10pF => 10.0e-12,
        }
    }
}

impl Rhd2000Registers {
    /// Generate a command list that writes a sine wave (or DC) to the
    /// impedance-test DAC (Register 6). Returns the number of commands
    /// in the resulting sequence.
    ///
    /// `frequency` = 0.0 produces a DC (flat mid-scale) output.
    /// `amplitude` is in DAC units (0..128, where 128 = full scale).
    ///
    /// Port of Intan `createCommandListZcheckDac`.
    pub fn create_command_list_zcheck_dac(
        &self,
        frequency: f64,
        amplitude: f64,
    ) -> Result<Vec<u16>, RhdCommandError> {
        let mut commands = Vec::with_capacity(MAX_COMMAND_LENGTH);

        if frequency <= 0.0 || self.sample_rate <= 0.0 {
            // DC: fill entire bank with WRITE(6, 128)
            for _ in 0..MAX_COMMAND_LENGTH {
                commands.push(reg_write(6, 128)?);
            }
            return Ok(commands);
        }

        // Compute how many samples make up one complete period of the
        // test waveform.  Clamp to at most MAX_COMMAND_LENGTH.
        let period_samples = (self.sample_rate / frequency).round() as usize;
        let period_samples = period_samples.clamp(1, MAX_COMMAND_LENGTH);

        let two_pi = 2.0 * std::f64::consts::PI;
        for i in 0..period_samples {
            let phase = two_pi * (i as f64) / (period_samples as f64);
            let dac_value = (amplitude * phase.sin() + 128.0).round() as i32;
            let dac_byte = dac_value.clamp(0, 255) as u8;
            commands.push(reg_write(6, dac_byte)?);
        }

        Ok(commands)
    }
}

fn rh1_from_upper_bandwidth(upper_bandwidth: f64) -> f64 {
    let log10f = f64::log10(upper_bandwidth);
    0.9730 * f64::powf(10.0, 8.0968 - 1.1892 * log10f + 0.04767 * log10f * log10f)
}

fn rh2_from_upper_bandwidth(upper_bandwidth: f64) -> f64 {
    let log10f = f64::log10(upper_bandwidth);
    1.0191 * f64::powf(10.0, 8.1009 - 1.0821 * log10f + 0.03383 * log10f * log10f)
}

fn rl_from_lower_bandwidth(lower_bandwidth: f64) -> f64 {
    let log10f = f64::log10(lower_bandwidth);
    if lower_bandwidth < 4.0 {
        1.0061
            * f64::powf(
                10.0,
                4.9391 - 1.2088 * log10f
                    + 0.5698 * log10f * log10f
                    + 0.1442 * log10f * log10f * log10f,
            )
    } else {
        1.0061 * f64::powf(10.0, 4.7351 - 0.5916 * log10f + 0.08482 * log10f * log10f)
    }
}
