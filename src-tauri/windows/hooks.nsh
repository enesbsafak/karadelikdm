; Karadelik Download Manager kurulum kancaları (Tauri NSIS şablonu, kullanıcı başına kurulum).
; Veriler (%APPDATA%\KaradelikDM: indirme listesi, ayarlar) kaldırmada silinmez.

!macro NSIS_HOOK_PREUNINSTALL
  ; Güncellemede (/UPDATE) değil, gerçek kaldırmada: tarayıcıların native messaging kayıtlarını sil.
  ; Kalırlarsa uzantı olmayan bir programı aramaya devam eder.
  ${If} $UpdateMode <> 1
    DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\dev.karadelik.dm"
    DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\dev.karadelik.dm"
    DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\dev.karadelik.dm"
    Delete "$APPDATA\KaradelikDM\dev.karadelik.dm.chrome.json"
    Delete "$APPDATA\KaradelikDM\dev.karadelik.dm.firefox.json"
    Delete "$APPDATA\KaradelikDM\bridge.json"
    ; Tercihlerden "Magnet ve .torrent için varsayılan yap" seçildiyse yazılan kayıtlar. Magnet anahtarı yalnız
    ; hâlâ bu kuruluma işaret ediyorsa silinir; başka bir torrent istemcisinin kaydına dokunulmaz.
    ReadRegStr $0 HKCU "Software\Classes\magnet\shell\open\command" ""
    StrCmp $0 '"$INSTDIR\karadelikdm.exe" "%1"' 0 +2
      DeleteRegKey HKCU "Software\Classes\magnet"
    ReadRegStr $0 HKCU "Software\Classes\.torrent" ""
    StrCmp $0 "KaradelikDM.torrent" 0 +2
      DeleteRegValue HKCU "Software\Classes\.torrent" ""
    DeleteRegValue HKCU "Software\Classes\.torrent\OpenWithProgids" "KaradelikDM.torrent"
    DeleteRegKey HKCU "Software\Classes\KaradelikDM.torrent"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Kurulum klasörü burada saklanır; kalırsa yeniden kurulum, silinmiş eski klasörü varsayılan sanar.
  ${If} $UpdateMode <> 1
    DeleteRegKey HKCU "Software\enesbsafak\Karadelik Download Manager"
    DeleteRegKey /ifempty HKCU "Software\enesbsafak"
  ${EndIf}
!macroend
