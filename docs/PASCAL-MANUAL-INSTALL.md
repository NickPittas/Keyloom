# Pascal GPUs (GTX 10-series) and Keyloom 2.5.0

## Current support

The distributed Keyloom 2.5.0 online installers use the pinned PyTorch 2.9.1/CUDA 12.8 runtime. They do not provide a validated Pascal-compatible runtime. Do not assume support for GTX 10-series, Titan X/Xp or other Pascal cards merely because an NVIDIA driver is installed.

A `no kernel image is available for execution on the device` error can indicate that a required CUDA component lacks kernels for the GPU. Different model families and native extensions also have their own compatibility requirements; changing only PyTorch does not establish full Keyloom compatibility.

See the [installation requirements](install.md#what-you-need) and [current user guide](user-guide.md). For a compatibility report, use [Keyloom issues](https://github.com/NickPittas/Keyloom/issues) and include the exact GPU, driver, payload ID, affected workflow and redacted error. Do not include credentials or private media.

## The old manual-swap instructions do not apply

An earlier version of this document described overwriting packages in `%LOCALAPPDATA%\EZ-CorridorKey\_internal`, checking a Qt **Help → Diagnostics** screen and relying on the old in-app updater. Those instructions targeted the former app layout, not current Keyloom.

Keyloom uses versioned application folders with a managed `runtime/python` tree, an install manifest and pinned native dependencies. Do **not** run the old `pip --target` commands against that tree: replacing torch/torchvision/triton independently can break native extensions and invalidate installer verification, repair and update assumptions. No replacement-wheel recipe is supported by this guide.

A Pascal-compatible variant would require its own consistent dependency lock, native-extension rebuilds and actual inference/installer verification on Pascal hardware. That work has not been established by the current release records.

If a managed runtime was modified manually, quit Keyloom and use the [online installer's repair procedure](install.md#updating-repairing-uninstalling) to restore the supported runtime. Repair restores the distributed dependencies; it does not add Pascal support. Projects, models and preferences are separate from application files and should not be deleted to troubleshoot a GPU compatibility error.
