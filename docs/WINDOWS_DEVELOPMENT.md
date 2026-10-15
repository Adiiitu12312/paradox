# Windows development builds and Application Control

**Current status:** the owner has turned Smart App Control off. The `b52ca2c`
Windows build passed a local smoke check at 13:54 UTC on October 15: Git parity,
one native watcher save, status, stop and process exit, all in a disposable
OS-temp repository. Node remains v26.4.0. No further security change is needed
for the development binary tested here.
The exact release from run `38744135728` has SHA-256
`229b16ee7384dd26d418c49ecda2fe5eb27c07e772e9850248d3e7d7d95215cf`.
Its watcher PID 26112 was absent after stop; the fixture was removed. Corrected
native timeline selection and cancellation also passed on all three OSes in
run `38746186331`, using the same release hashes as the performance gates.

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
the policy. The owner subsequently confirmed turning Smart App Control off.
A local smoke check at 10:36 UTC on October 15 ran the Windows artifact from
run `38719356751` (source `e8f0f55`) through existing Node v26.4.0 in a disposable
OS-temp repository. Version/status matched Git; a manually started background
watcher captured exactly one save with its message; stop released the session
and the watcher process exited. The temporary repository was removed afterward.
Local development execution is therefore unblocked; no Node change was needed.
That run's macOS performance failure remains a separate acceptance issue.

The newer `c79377a` Windows artifact from run `38727716484` repeated this local
smoke check at 11:37 UTC: Git parity, one native watcher save with its message,
status and stop all passed, with no watcher process afterward. Node remained
v26.4.0. The hosted macOS timing and Windows terminal-fixture failures remain
recorded separately in the M4.5 report.

UI work will use the existing local Node/npm and browser when its milestone
arrives. Minimal local Rust tools are permitted only if that work needs them;
the large C++/Windows SDK toolchain remains excluded. These development builds
are not yet recommended for the main working repository: release hardening is
still ahead. M4 closes with an approved macOS timing exception; the CI gate stays
strict, as recorded in [the M4 report](milestones/M4.md).
