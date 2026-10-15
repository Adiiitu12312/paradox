# Windows development builds and Application Control

On October 15, 2026, Windows Code Integrity event 3077 recorded
`C:\Program Files\nodejs\node.exe` trying to launch the downloaded Paradox
development binary. The blocked file was `paradox.exe`, which did not meet the
active signing policy. The installed Node signature verified as valid, signed
by OpenJS Foundation. No Node upgrade, downgrade or reinstall is needed for
this observed block.

To inspect the setting yourself:

1. Open **Windows Security → App & browser control → Smart App Control settings**.
2. If Smart App Control is **On**, it has no per-app Allow exception. To run
   unsigned development builds, you can choose **Off**. This changes application
   trust protection across the PC; leave Microsoft Defender antivirus enabled.
   A build signed with a trusted code-signing certificate is the alternative.
3. If Smart App Control is already **Off**, or the setting is managed by an
   administrator, leave it unchanged and inspect the actual blocking policy.
   Smart App Control is not confirmed solely by the Code Integrity event; other
   application policies can also enforce signing requirements.
4. **Windows Security → Virus & threat protection → Protection history** shows
   Defender actions. Application Control blocks may instead appear in **Event
   Viewer → Applications and Services Logs → Microsoft → Windows →
   CodeIntegrity → Operational**, event **3077**. Check the blocked executable
   path separately from the process that tried to launch it.

Microsoft documents the lack of individual Smart App Control exceptions in its
[Smart App Control FAQ](https://support.microsoft.com/en-us/windows/security/threat-malware-protection/smart-app-control-frequently-asked-questions).
See also [Protection History](https://support.microsoft.com/en-us/windows/security/windows-security/protection-history-in-the-windows-security-app).

The agent has not changed these settings, added antivirus exclusions or bypassed
the policy. Hosted CLI checks can continue independently. After you change an
applicable setting yourself, say so before another local test is attempted.

UI work will use the existing local Node/npm and browser when its milestone
arrives. Minimal local Rust tools are permitted only if that work needs them;
the large C++/Windows SDK toolchain remains excluded. These development builds
are not yet recommended for the main working repository: M4 acceptance is open.
