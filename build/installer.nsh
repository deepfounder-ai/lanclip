; Remove Lanclip's Explorer context-menu verbs (written per-user by the app on first run).
!macro customUnInstall
  DeleteRegKey HKCU "Software\Classes\*\shell\Lanclip.Send"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\Lanclip.Send"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\Lanclip.Paste"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\Lanclip.Paste"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\Lanclip.Clip"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Lanclip"
!macroend
