@echo off
cd /d "%~dp0"
echo ========================================
echo  Rust 版打包 (release, ~310KB)
echo ========================================
cargo build --release
if errorlevel 1 ( echo BUILD FAILED & pause & exit /b 1 )
mkdir release 2>nul
copy /Y target\release\srun.exe release\ >nul
echo.
echo Done: %~dp0release\srun.exe
echo 注意: 运行需在 release\ 下放 config.json(复制 config.example.json 改名并填账号)
pause
