# Pekora: Reborn Client Bootstrapper

A client bootstrapper for **Pekora: Reborn**, written in Rust.

## Requirements

- [Visual Studio 2026](https://visualstudio.microsoft.com/)
- **Workload:** Desktop development with C++

### Required Individual Components

Under **Individual components** in the Visual Studio Installer, ensure the following are installed:

- MSVC v143 or newer, C++ x64/x86 build tools
- Windows 10 or Windows 11 SDK
- C++ CMake tools for Windows *(optional)*

## Building

Open a Developer PowerShell for Visual Studio, navigate to the project directory, and run:

```powershell
cargo build
```

For an optimized release build:

```powershell
cargo build --release
```
