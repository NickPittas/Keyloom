from __future__ import annotations

import logging
import os
import re
import sys

from PySide6.QtWidgets import QWidget, QLabel, QMessageBox
from PySide6.QtCore import Qt, Slot, QSettings

from . import _tr

from ui.widgets.preferences_dialog import (
    PreferencesDialog, KEY_SHOW_TOOLTIPS, DEFAULT_SHOW_TOOLTIPS,
    KEY_TRACKER_MODEL, DEFAULT_TRACKER_MODEL,
    KEY_UI_LANGUAGE,
    KEY_SHOW_UPDATE_BUTTON, DEFAULT_SHOW_UPDATE_BUTTON,
    get_setting_bool, get_setting_str, get_setting_int,
)

logger = logging.getLogger(__name__)

# TEMPORARY: keep a visible tester build identifier on the user-test
# branch so remote testers can confirm they pulled the right build. Remove this
# before merging the branch back into main.
_SHOW_TESTER_BUILD_ID = False


class SettingsMixin:
    """Layout, preferences, about dialog, and update methods for MainWindow."""

    def _reset_layout(self) -> None:
        self._splitter.setSizes([920, 280])
        self._vsplitter.setSizes([600, 140])

    def _toggle_queue_panel(self) -> None:
        self._queue_panel.toggle_collapsed()

    def _toggle_debug_console(self) -> None:
        """Toggle the in-app debug console (F12)."""
        if self._debug_console.isVisible():
            self._debug_console.hide()
        else:
            self._debug_console.show()

    def _run_startup_diagnostics(self, device: str) -> None:
        """Check environment for known issues and show a diagnostic dialog."""
        from ui.widgets.diagnostic_dialog import run_startup_diagnostics, StartupDiagnosticDialog
        issues = run_startup_diagnostics(device)
        if issues:
            dlg = StartupDiagnosticDialog(issues, parent=self)
            dlg.exec()

    def _show_preferences(self) -> None:
        """Toggle the Preferences dialog (Ctrl+,)."""
        if self._prefs_dialog is not None:
            self._prefs_dialog.reject()
            return
        old_lang = get_setting_str(KEY_UI_LANGUAGE, "en")
        dlg = PreferencesDialog(self)
        self._prefs_dialog = dlg
        accepted = dlg.exec() == PreferencesDialog.Accepted
        self._prefs_dialog = None
        if accepted:
            self._apply_tooltip_setting()
            self._apply_update_button_setting()
            self._apply_sound_setting()
            self._apply_tracker_model_setting()
            self._apply_language_setting(old_lang)

    def _apply_language_setting(self, old_lang: str) -> None:
        """Apply a language change immediately by reloading the UI.

        Strings are baked into widgets at construction time, so a live
        switch swaps the QTranslator and rebuilds the main window. The
        session is saved on close and restored by the new window, so the
        open project, selected clip, and parameters survive the swap.
        Skipped while jobs are running: tearing down the window mid-job
        would orphan worker signals.
        """
        new_lang = get_setting_str(KEY_UI_LANGUAGE, "en")
        if new_lang == old_lang:
            return

        busy = (
            self._active_job_id is not None
            or self._service.job_queue.has_pending
            or self._extract_worker.is_busy
        )
        if busy:
            QMessageBox.information(
                self, _tr("Language"),
                _tr("The interface language will change after the current "
                    "jobs finish and the app restarts."),
            )
            return

        from PySide6.QtWidgets import QApplication
        from ui.app import apply_language
        app = QApplication.instance()
        if not apply_language(app, new_lang):
            QMessageBox.warning(
                self, _tr("Language"),
                _tr("Could not load the selected language. "
                    "The interface stays in English."),
            )
            return

        logger.info("Language changed %s -> %s, reloading main window", old_lang, new_lang)
        self._reload_main_window()

    def _reload_main_window(self) -> None:
        """Tear down this window and build a fresh one in the new language.

        The shared service and recent-sessions store carry over; the open
        project is reopened through the normal session-restore path.
        """
        from PySide6.QtWidgets import QApplication
        from ui.main_window import MainWindow

        app = QApplication.instance()
        clips_dir = self._clips_dir
        geometry = self.geometry()

        # Keep the app alive while no window exists
        prev_quit = app.quitOnLastWindowClosed()
        app.setQuitOnLastWindowClosed(False)
        try:
            # closeEvent saves the session and stops this window's workers
            self.close()
            new_window = MainWindow(self._service, self._recent_store)
            new_window.setGeometry(geometry)
            new_window.show()
            if clips_dir and os.path.isdir(clips_dir):
                new_window._on_clips_dir_changed(clips_dir, skip_session_restore=False)
        finally:
            app.setQuitOnLastWindowClosed(prev_quit)

    def _show_hotkeys(self) -> None:
        """Open the Hotkeys configuration dialog and apply changes."""
        from ui.widgets.hotkeys_dialog import HotkeysDialog
        dlg = HotkeysDialog(self._shortcut_registry, self)
        if dlg.exec() == HotkeysDialog.Accepted:
            self._setup_shortcuts()

    def _show_download_manager(self) -> None:
        """Open the Download Manager (SetupWizard) on demand.

        Unlike the first-launch path, this entry is always available so users
        can re-scan their install folder, install optional models they
        skipped earlier, or point at a different folder.
        """
        from ui.widgets.setup_wizard import SetupWizard
        dlg = SetupWizard(parent=self)
        dlg.setWindowTitle(_tr("Download Manager"))
        dlg.exec()

    def _apply_tooltip_setting(self) -> None:
        """Enable or disable tooltips globally based on saved preference."""
        show = get_setting_bool(KEY_SHOW_TOOLTIPS, DEFAULT_SHOW_TOOLTIPS)
        if not show:
            # Disable: clear all tooltips on the main window tree
            for w in self.findChildren(QWidget):
                w.setToolTip("")
        # If enabled, tooltips stay as set during widget construction.
        # A full re-enable would require rebuilding tooltip strings, which
        # is unnecessary — the setting takes full effect on next app launch.

    def _apply_sound_setting(self) -> None:
        """Apply UI sounds on/off and volume from saved preferences."""
        from ui.widgets.preferences_dialog import KEY_UI_SOUNDS, DEFAULT_UI_SOUNDS
        from ui.sounds.audio_manager import UIAudio
        UIAudio.set_muted(not get_setting_bool(KEY_UI_SOUNDS, DEFAULT_UI_SOUNDS))
        # Restore volume level
        vol = QSettings().value("ui/sounds_volume", 1.0, type=float)
        UIAudio.set_volume(vol)
        if hasattr(self, "_volume_control"):
            self._volume_control.sync_mute_state()

    def _apply_tracker_model_setting(self) -> None:
        """Apply saved SAM2 tracker model preference to the backend service."""
        model_id = get_setting_str(KEY_TRACKER_MODEL, DEFAULT_TRACKER_MODEL)
        self._service.set_sam2_model(model_id)

    def _show_report_issue(self) -> None:
        import logging as _logging

        from ui.widgets.report_issue_dialog import ReportIssueDialog

        # Gather GPU info from last monitor update
        gpu_info = {}
        if hasattr(self, "_last_vram_info"):
            gpu_info = self._last_vram_info

        # Gather recent WARNING/ERROR log lines from debug console buffer
        recent_errors: list[str] = []
        if hasattr(self, "_debug_console"):
            for html, levelno in self._debug_console._log_buffer:
                if levelno >= _logging.WARNING:
                    plain = re.sub(r"<[^>]+>", "", html).strip()
                    if plain:
                        recent_errors.append(plain)
                if len(recent_errors) >= 20:
                    break

        dlg = ReportIssueDialog(
            gpu_info=gpu_info,
            recent_errors=recent_errors,
            parent=self,
        )
        dlg.exec()

    def _show_about(self) -> None:
        app_version = self._get_local_version()
        box = QMessageBox(self)
        box.setWindowTitle(_tr("About EZ-CorridorKey"))
        box.setTextFormat(Qt.RichText)
        accent = getattr(self, "_current_accent", "#2CC350")
        box.setText(
            f'<h2><a href="https://www.ezcorridorkey.com" '
            f'style="color: #FFF203; text-decoration: none;">'
            f'EZ<span style="color:{accent};">-</span>Corridor'
            f'<span style="color:{accent};">Key</span></a>'
            f" v{app_version}</h2>"
            "<p>" + _tr("AI Green Screen Keyer") + "<br>"
            '<a href="https://github.com/nikopueringer/CorridorKey#corridorkey-licensing-and-permissions">'
            "CC BY-NC-SA 4.0 License</a></p>"
            "<p><b>" + _tr("Special Thanks") + "</b></p>"
            "<p>"
            '<a href="https://github.com/nikopueringer/">Niko Pueringer</a> — OG CorridorKey Creator<br>'
            '<a href="https://www.edzisk.com">Ed Zisk</a> — Maintainer, GUI, workflow, SFX, QA<br><br>'
            '<a href="https://www.clade.design/">Sara Ann Stewart</a> — Logo<br>'
            '<a href="https://github.com/Raiden129">Jhe Kim</a> — Hiera optimization<br>'
            '<a href="https://github.com/MarcelLieb">MarcelLieb</a> — Tiling optimization<br>'
            '<a href="https://github.com/99oblivius">99oblivius</a> — FX graph cache (<a href="https://github.com/99oblivius/CorridorKey-Engine">CorridorKey-Engine</a>)<br>'
            '<a href="https://github.com/Warwlock">Warwlock</a> — BiRefNet integration<br>'
            '<a href="https://github.com/DCRepublic">DCRepublic</a> — Docker integration'
            "</p>"
            "<p>"
            '<a href="https://patreon.com/edenaion" style="color: #FF0000; '
            'text-decoration: none; font-weight: bold;">'
            "Subscribe on Patreon</a><br>"
            '<a href="https://ko-fi.com/edenaion" style="color: #FF5915; '
            'text-decoration: none; font-weight: bold;">'
            "\u2615 Support on Ko-fi</a>"
            "</p>"
        )
        # QMessageBox uses an internal QLabel — find it and enable clickable links
        for label in box.findChildren(QLabel):
            label.setOpenExternalLinks(True)
        box.exec()

    # ── Update Check ──────────────────────────────────────────

    def _get_local_version(self) -> str:
        import tomllib
        candidates = []
        if getattr(sys, 'frozen', False):
            candidates.append(os.path.join(sys._MEIPASS, "pyproject.toml"))
        candidates.append(
            os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(__file__))), "pyproject.toml")
        )
        for path in candidates:
            try:
                with open(path, "rb") as f:
                    return tomllib.load(f)["project"]["version"]
            except Exception:
                continue
        return "0.0.0"

    def _get_git_short_hash(self) -> str:
        try:
            import subprocess

            repo_root = os.path.dirname(os.path.dirname(__file__))
            result = subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                cwd=repo_root,
                capture_output=True,
                text=True,
                check=True,
                timeout=2,
            )
            return result.stdout.strip()
        except Exception:
            return ""

    def _get_visible_build_id(self) -> str:
        version = self._get_local_version()
        if not _SHOW_TESTER_BUILD_ID:
            return f"v{version}"

        git_hash = self._get_git_short_hash()
        if git_hash:
            return f"v{version} test ({git_hash})"
        return f"v{version} test"

    def _check_for_updates(self) -> None:
        """No-op: EZ-CorridorKey2 never checks upstream for updates."""
        return

    @Slot(str)
    def _on_update_available(self, remote_version: str) -> None:
        """Never show an update button in the CorridorKey 2.0 fork."""
        if hasattr(self, "_update_btn"):
            self._update_btn.setVisible(False)

    def _apply_update_button_setting(self) -> None:
        """The update button is permanently hidden in this fork."""
        if hasattr(self, "_update_btn"):
            self._update_btn.setVisible(False)


    def _position_update_btn(self) -> None:
        """Center the floating update button in the brand bar, below the menu bar.

        The brand bar has the brand mark on the far left and the GPU/VRAM meter
        on the far right, so its horizontal center is empty. Pinning here keeps
        the button from covering the VRAM meter (issue #176).
        """
        if not hasattr(self, '_update_btn') or not self._update_btn.isVisible():
            return
        btn = self._update_btn
        margin = 8
        centered = (self.width() - btn.width()) // 2
        right_clamp = max(margin, self.width() - btn.width() - margin)
        x = min(max(margin, centered), right_clamp)
        y = self.menuBar().height() + margin
        btn.move(x, y)
        btn.raise_()


    def _run_update(self) -> None:
        """Updates are disabled: upstream releases would overwrite the v2 engine."""
        QMessageBox.information(
            self, _tr("Updates disabled"),
            _tr("Updates are disabled in this CorridorKey 2.0 build of "
                "EZ-CorridorKey2. Upstream EZ-CorridorKey updates would "
                "replace the CorridorKey 2.0 engine with CorridorKey v1."),
        )
