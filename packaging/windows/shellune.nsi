; NSIS installer for Shellune (Windows x86_64).
;
; This script has never been compiled on the packaging machine used for the 0.1.0
; verification run: NSIS is not installed there. It is the reviewed packaging
; definition, not evidence that an installer was produced. Compile it with:
;
;     makensis /DVERSION=0.4.0 /DBINARY=..\..\target\release\shellune.exe ^
;              /DOUTFILE=dist\Shellune-0.4.0-setup.exe packaging\windows\shellune.nsi
;
; Optional code signing (only when a certificate exists):
;
;     makensis /DSIGNTOOL="C:\Program Files (x86)\Windows Kits\10\bin\x64\signtool.exe" ...
;
; The installer writes per user, so it needs no administrator rights and does not
; touch another account's files.

Unicode true
!include "MUI2.nsh"
!define MUI_ICON "..\..\assets\icons\shellune.ico"
!define MUI_UNICON "..\..\assets\icons\shellune.ico"

!ifndef VERSION
  !define VERSION "0.4.0"
!endif
!ifndef BINARY
  !define BINARY "..\..\target\release\shellune.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\..\dist\Shellune-${VERSION}-setup.exe"
!endif

Name "Shellune"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Shellune"
InstallDirRegKey HKCU "Software\Shellune" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Shellune" SecMain
  SetOutPath "$INSTDIR"
  File "${BINARY}"
  File "..\..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Shellune.lnk" "$INSTDIR\shellune.exe"
  WriteRegStr HKCU "Software\Shellune" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Shellune" \
      "DisplayName" "Shellune"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Shellune" \
      "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Shellune" \
      "UninstallString" "$INSTDIR\uninstall.exe"
!ifdef SIGNTOOL
  !finalize '${SIGNTOOL} sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 "%1"'
!endif
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\shellune.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Shellune.lnk"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Shellune"
  DeleteRegKey HKCU "Software\Shellune"
  ; Saved hosts and accepted server identities are user data, not program files, and
  ; are left in place on purpose.
SectionEnd
