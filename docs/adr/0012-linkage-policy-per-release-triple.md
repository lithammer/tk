# Linkage policy per release triple

Release binaries use these linkage rules:

- musl targets are fully static for containers and Alpine;
- glibc is dynamic with a 2.28 floor for RHEL 8, Debian 10, and Ubuntu 18.04;
- macOS links `libSystem` dynamically with `-mmacos-version-min=11.0`, because
  Apple does not support static linkage to it; and
- Windows uses `-static-libgcc` with dynamic `msvcrt`, so `tk.exe` ships
  without `libgcc_s_seh-1.dll`.

The glibc floor trades newer libc features for support on older distributions.
Static libgcc keeps the Windows release to one file.
