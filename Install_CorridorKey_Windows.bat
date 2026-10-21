@echo off
setlocal enabledelayedexpansion
TITLE CorridorKey 2.0 Setup Wizard
echo ===================================================
echo     CorridorKey - Windows Auto-Installer
echo ===================================================
echo.

:: 1. Check Python
echo [1/4] Checking Python...
python --version >nul 2>&1
if %errorlevel% neq 0 (
    echo   [ERROR] Python not found. Install Python 3.11+ from https://python.org
    echo   Make sure to check "Add Python to PATH" during installation.
    pause
    exit /b 1
)
for /f "tokens=2" %%V in ('python --version 2^>^&1') do echo   [OK] Python %%V
python -c "import sys; sys.exit(0 if (3,11)<=sys.version_info[:2]<(3,14) else 1)" >nul 2>&1
if %errorlevel% neq 0 (
    echo   [ERROR] CorridorKey 2.0 needs Python 3.11-3.13.
    pause
    exit /b 1
)

REM CorridorKey 2.0 is CUDA/NVIDIA only (no CPU keying path).
nvidia-smi >nul 2>&1
if %errorlevel% neq 0 (
    echo   [ERROR] No working NVIDIA GPU detected ^(nvidia-smi missing or failing^).
    echo   CorridorKey 2.0 requires an NVIDIA GPU with CUDA. Install the NVIDIA driver and re-run.
    pause
    exit /b 1
)

:: 2. Create Virtual Environment
echo.
echo [2/4] Setting up Python Virtual Environment (venv)...
if not exist "venv\Scripts\activate.bat" (
    python -m venv venv
) else (
    echo   Virtual environment already exists.
)

:: 3. Detect CUDA + Install Dependencies
echo.
echo [3/4] Installing Dependencies (This might take a while)...
call venv\Scripts\activate.bat
python -m pip install --upgrade pip >nul 2>&1

pip install --extra-index-url https://download.pytorch.org/whl/cu128 -e .
if %errorlevel% neq 0 (
    echo   [ERROR] pip install failed
    pause
    exit /b 1
)

:: 4. Download Weights
echo.
echo [4/4] Downloading CorridorKey 2.0 Model Weights (~5 GB)...
python scripts\setup_models.py --corridorkey
if %errorlevel% neq 0 (
    echo   [WARN] Model download failed. Retry later: python scripts\setup_models.py --corridorkey
)

echo.
echo ===================================================
echo   Setup Complete! You are ready to key!
echo   Drag and drop folders onto CorridorKey_DRAG_CLIPS_HERE_local.bat
echo ===================================================
pause
