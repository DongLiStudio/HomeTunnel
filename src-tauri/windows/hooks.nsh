; Registration and Start Menu entries are provided by Tauri's installer template.
; Start in the interactive user's context to repair associations and respect saved preferences.
!macro NSIS_HOOK_POSTINSTALL
  nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "--background"
!macroend
