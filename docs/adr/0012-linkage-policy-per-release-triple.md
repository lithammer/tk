# Linkage policy per release triple

Release binaries link per triple: musl fully static
(containers/Alpine), glibc dynamic with a 2.28 floor (RHEL 8 / Debian 10
/ Ubuntu 18.04), macOS dynamic `libSystem` with
`-mmacos-version-min=11.0` (Apple forbids static linking it), and
Windows `-static-libgcc` with dynamic `msvcrt` so `tk.exe` ships as a
single file rather than as `tk.exe` + `libgcc_s_seh-1.dll`. The glibc
floor and the Windows static-libgcc choices are the substantive
trade-offs: modernness for older-distro portability, and one shipped
file instead of two.
