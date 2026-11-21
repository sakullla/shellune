; NSIS installer for Shellune (Windows x86_64).
;
; The release workflow compiles it on windows-latest with NSIS 3 (choco install
; nsis). Compile it locally with:
;
;     makensis /DVERSION=0.4.16 /DBINARY=..\..\target\release\shellune.exe ^
;              /DOUTFILE=dist\Shellune-0.4.16-setup.exe packaging\windows\shellune.nsi
;
; Optional code signing (only when a certificate exists):
;
;     makensis /DSIGNTOOL="C:\Program Files (x86)\Windows Kits\10\bin\x64\signtool.exe" ...
;
; The installer writes per user, so it needs no administrator rights and does not
; touch another account's files.
;
; Command line (the in-app updater starts it with "/S /R"):
;
;     /S   silent: no pages, the default or previously used folder (MUI built in)
;     /R   start the installed Shellune after a successful install
;     /D=<folder>  install folder (NSIS built in; must come last)
;
; Before any file is written the installer waits for the installed shellune.exe
; to exit (the updater quits right after starting setup), checking every 100 ms for
; about 10 seconds. If it is still running then, an interactive install asks to
; close it and a silent one closes it (taskkill /F /IM shellune.exe).
;
; This file is UTF-8 with a byte order mark, which makensis needs to read the
; Chinese strings below; keep the mark when editing.

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!define MUI_ICON "..\..\assets\icons\shellune.ico"
!define MUI_UNICON "..\..\assets\icons\shellune.ico"

!ifndef VERSION
  !define VERSION "0.4.16"
!endif
!ifndef BINARY
  !define BINARY "..\..\target\release\shellune.exe"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\..\dist\Shellune-${VERSION}-setup.exe"
!endif

; About 10 seconds of 100 ms checks for Shellune to exit, then 5 more after closing it.
!define WAIT_TICKS 100
!define KILL_WAIT_TICKS 50

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

LangString ShelluneRunning ${LANG_ENGLISH} \
    "Shellune is still running. Choose OK to close it and continue, or Cancel to stop setup."
LangString ShelluneRunning ${LANG_SIMPCHINESE} \
    "Shellune 仍在运行。选择“确定”将其关闭并继续安装，选择“取消”退出安装。"
LangString ShelluneLocked ${LANG_ENGLISH} \
    "Shellune could not be closed, so nothing was installed. Close it and run setup again."
LangString ShelluneLocked ${LANG_SIMPCHINESE} \
    "无法关闭 Shellune，未安装任何内容。请关闭后重新运行安装程序。"

; "1" when /R was given: start Shellune after a successful install.
Var Relaunch

Function .onInit
  StrCpy $Relaunch "0"
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/R" $R1
  ${IfNot} ${Errors}
    StrCpy $Relaunch "1"
  ${EndIf}
FunctionEnd

; $R0 = "1" when $INSTDIR\shellune.exe is running: Windows refuses to open the
; image of a running program for writing. Opening for append changes nothing.
Function ShelluneInUse
  StrCpy $R0 "0"
  ${If} ${FileExists} "$INSTDIR\shellune.exe"
    ClearErrors
    FileOpen $R1 "$INSTDIR\shellune.exe" a
    ${If} ${Errors}
      StrCpy $R0 "1"
    ${Else}
      FileClose $R1
    ${EndIf}
  ${EndIf}
FunctionEnd

; Waits up to $R3 ticks of 100 ms for Shellune to exit; $R0 = "1" if it has not.
Function WaitTicks
  StrCpy $R2 0
  ${Do}
    Call ShelluneInUse
    ${If} $R0 == "0"
    ${OrIf} $R2 >= $R3
      ${ExitDo}
    ${EndIf}
    Sleep 100
    IntOp $R2 $R2 + 1
  ${Loop}
FunctionEnd

; Makes sure the installed copy is not running before its files are replaced.
Function WaitForShellune
  StrCpy $R3 ${WAIT_TICKS}
  Call WaitTicks
  ${If} $R0 == "0"
    Return
  ${EndIf}
  DetailPrint "Shellune is still running after about 10 seconds"
  ; A silent install takes the default answer (OK): it closes Shellune.
  MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "$(ShelluneRunning)" /SD IDOK IDOK close
  Abort
  close:
  DetailPrint "Closing Shellune (taskkill /F /IM shellune.exe)"
  nsExec::Exec 'taskkill /F /IM shellune.exe'
  Pop $R4
  StrCpy $R3 ${KILL_WAIT_TICKS}
  Call WaitTicks
  ${If} $R0 == "1"
    MessageBox MB_OK|MB_ICONSTOP "$(ShelluneLocked)" /SD IDOK
    Abort
  ${EndIf}
FunctionEnd

Section "Shellune" SecMain
  Call WaitForShellune
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

; /R: start the new version. The installer runs as the user, so Shellune does too.
Function .onInstSuccess
  ${If} $Relaunch == "1"
    Exec '"$INSTDIR\shellune.exe"'
  ${EndIf}
FunctionEnd

; /R and the install failed: the updater already quit, so start whatever copy is
; still in place rather than leave the user without Shellune.
Function .onInstFailed
  ${If} $Relaunch == "1"
    Call ShelluneInUse
    ${If} $R0 == "0"
    ${AndIf} ${FileExists} "$INSTDIR\shellune.exe"
      Exec '"$INSTDIR\shellune.exe"'
    ${EndIf}
  ${EndIf}
FunctionEnd

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
