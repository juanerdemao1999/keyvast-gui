# Opal Kelly FrontPanel runtime (third-party redistributable)

This directory bundles the Opal Kelly **FrontPanel** runtime library so the
Windows build of `kv-gui` / `kv-cli` can talk to the XEM7310-A75 FPGA board
without a separate FrontPanel install.

```
windows-x64/okFrontPanel.dll   FrontPanel runtime DLL (x86-64, Windows)
```

## Licensing

`okFrontPanel.dll` is **proprietary software owned by Opal Kelly Incorporated**.
It is redistributed here under the terms of the Opal Kelly FrontPanel SDK
license that accompanies the SDK download — it is **not** covered by this
repository's own license, and the project maintainers claim no ownership of it.

- Vendor: Opal Kelly Incorporated — https://opalkelly.com
- Product: FrontPanel SDK (runtime component)
- Redistribution: permitted for applications built against the FrontPanel SDK,
  per the SDK's "Distributable Code" terms.

If you redistribute builds of this project that include this DLL, you must
comply with the Opal Kelly FrontPanel SDK license. To obtain the SDK, its
headers, and the full license text, register at the Opal Kelly download portal.

## Provenance / updating

The DLL is taken verbatim from the official FrontPanel SDK for Windows x64.
When upgrading, replace the file with the matching version from a fresh SDK
download and verify the board still enumerates (`kv-cli`'s RHD smoke path or
the GUI DEVICE panel). Do not modify the binary.
