; Hooks Tauri's NSIS installer runs (bundle.windows.nsis.installerHooks).

!macro NSIS_HOOK_POSTINSTALL
  ; A silent or passive install creates the desktop shortcut right after it
  ; writes Haul.exe, and Explorer can draw the shortcut while the new file is
  ; still locked (by an antivirus scan, say), then keep showing a generic icon.
  ; SHCNE_ASSOCCHANGED makes Explorer read icons again.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend
