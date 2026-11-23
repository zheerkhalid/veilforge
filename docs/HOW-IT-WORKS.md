# How VeilForge works, and its honest limits

VeilForge makes files and folders inaccessible or unfindable **for the Windows
that is running**, using ordinary NTFS features. It does **not** encrypt
anything. Read the limits below before you rely on it.

## The three modes

**Lock (visible, no access).** The item stays where it is. VeilForge saves its
permissions, then replaces them with a protected permission list (one that no
longer inherits from the folder above) with two entries: "deny Everyone" for
reading, writing, listing, running, deleting and changing attributes, and one
entry that lets your own account see that the lock is there and take it off
again. For a folder, everything inside gets the same list, because Windows
would otherwise let a program open a file deep inside by its full path. The
owner is not changed. Opening, listing, copying or deleting the item gives
"Access is denied".

**Hide.** The item is moved into a holding folder on the same drive
(`<drive>\VeilForge.sys\`, or a per-user folder on the same drive if the drive
root cannot be written) and renamed to a random name without an extension. The
move is a rename on the same drive: instant, no copy, the content is never
rewritten. The holding folder is access-denied, hidden + system, and excluded
from Windows indexing. Explorer, Explorer search, the Windows index and tools
that read the NTFS file table directly show at most a meaningless random entry
inside a protected folder. The original name, extension and place are stored
only inside the encrypted map.

For a hidden **folder**, the names of the files inside it would still sit in
the file table under their own names. "Also scramble names inside" renames
every child to a random name as well (and back again on restore). It is on by
default for folders of ordinary size; for huge trees it takes time, because
every entry is renamed once.

**Ask for password on open.** The item is hidden (or locked) as above and a
shortcut with the original name and icon is left in its place. Double-clicking
it asks for the master password, puts the item back, opens it with its normal
program and locks it again when that program exits or the folder window is
closed, after the idle time, when Windows locks, sleeps or you sign out, or
when you choose "Lock now" from the tray icon. The shortcut necessarily shows
the original name: that is the purpose of this mode.

## Locking again

Items you opened "for now" are protected again on their own: after the idle
time you chose, when Windows locks, sleeps or you sign out, and whenever you
ask. There are four ways to ask, and all of them lock every open item and
VeilForge itself at once:

- "Lock now" in the tray menu;
- the **lock hotkey**, if you set one in Settings (for example Ctrl+Alt+L);
- `VeilForge.exe --lock-now`, for a shortcut, a script or the Task Scheduler;
- "Lock them now" on the lock screen, which appears there whenever something
  is still open. Locking never needs the master password.

The automatic timers only run while VeilForge is running (in the tray is
enough). An item that a program still has open cannot be locked; VeilForge
says so and leaves it open rather than forcing anything.

Closing the window to the tray also locks VeilForge itself (the list), unless
you switch that off. Ctrl+L does the same while the window is in front.

## The wrong-password log

Every wrong master password is noted in `attempts.json` in the data folder:
the time, and where it was typed (the VeilForge window, a protected item's
password prompt, "Restore everything"). **What was typed is never stored.**
After the next unlock, VeilForge tells you once if there were wrong tries
since your last unlock, leaving out the slips you just made yourself. The
waiting time after wrong passwords counts all of them together and is not
reset by closing the program.

The log has to be writable without the master password, so it is plain text:
someone who can write to your data folder can also empty it. Treat it as a
hint that somebody tried, not as proof that nobody did.

## The map and the master password

The only sensitive data VeilForge keeps is the **map**: original path, random
name, mode, saved permissions, owner, attributes, dates and the child rename
table. It is encrypted with a key derived from your master password (Argon2id,
then XChaCha20-Poly1305), written atomically, with rolling backups, in
`%APPDATA%\VeilForge`. A small encrypted copy of each item's own map entry is
also kept in the holding folder, so losing the app-data folder is not fatal:
"Rebuild from recovery copies" needs only the master password.

**There is no password recovery.** Without the master password nobody,
including the author, can read the map. Your files are still on the disk,
unencrypted, under random names in the holding folders, and an administrator
can get at their content, but the original names and places are gone.

Every step of every operation is written to a journal first, so a crash or
power cut in the middle is finished or undone the next time the map is opened.

## What it does not protect against

- **Administrators.** An administrator can take ownership of a locked item or
  of the holding folder and reset its permissions. VeilForge raises the bar for
  normal use of this Windows account; it is not a wall against someone with
  administrator rights and the knowledge to use them.
- **Another operating system.** Booting from a USB stick, moving the disk to
  another PC, or reading it from Linux ignores Windows permissions entirely.
- **Disk and forensic tools** that read the raw disk.
- **No encryption.** The content of your files is exactly as it was. If the
  content must stay secret from someone with physical or administrator access,
  use real encryption (BitLocker, an encrypted container) instead of, or in
  addition to, VeilForge.
- **Traces of names elsewhere.** Hiding an item cannot remove what Windows and
  other programs already noted: recent-files lists and jump lists, thumbnail
  caches, the search index until it catches up, shortcuts, "open with" history,
  program logs, the NTFS change journal, and your **backups** or File History,
  which may still hold the item under its real name.
- **A locked item can still be renamed or moved.** Measured on NTFS: deleting
  a locked file or folder directly, overwriting it, and replacing it by
  renaming another file over it are all refused with "Access is denied". But
  NTFS lets the owner of the *containing* folder rename an entry in it even
  when the entry itself denies everything, so a locked file or folder can be
  renamed or moved elsewhere on the same drive. It stays locked wherever it
  goes and nothing in it can be read, but VeilForge then reports it as
  missing until it is moved back. Sending it to the Recycle Bin is such a move
  too (this follows from the measurement; it was not tested separately). If
  that matters, use Hide: a hidden item sits inside the access-denied holding
  folder and cannot be reached at all.
- **FAT / exFAT drives** have no permissions. Only Hide is offered there, with
  reduced protection: the holding folder is merely hidden, not access-denied.
- **Network and cloud-synced folders.** Hiding moves the item away, so a sync
  client sees a deletion and may delete it from the cloud and your other
  devices. VeilForge warns before you do this.

## Safety rules

VeilForge refuses to protect the Windows folder, Program Files, ProgramData,
your profile folder itself, other users' profiles, drive roots, its own program
and data folders, and anything that contains one of those. Items never move
across drives. Files that are open in a program are reported, not forced.

## If something goes wrong

- **Health check** verifies that every item is where the map says, and repairs
  what it can.
- **Panic restore** (Health check page, or `VeilForge.exe --restore-all`) asks
  for the master password and puts every item back exactly as it was: place,
  name, permissions, owner, attributes and dates.
- **Uninstalling** runs the same restore first. If you uninstall anyway, items
  stay protected; install VeilForge again and enter the password to restore.

VeilForge is made by Zheer Khalid (zheer.dev) and released under the MIT License.
