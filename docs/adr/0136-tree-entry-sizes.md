# ADR 0136 — Tree entries carry the size of files

Status: Accepted — 2026-10-31. Amends 0043, 0059.

## Context

The Files view shows the size of each file and must decide, before downloading anything, whether a file can be shown. `GetBlob` refuses files above 1 MiB (0047), and the only way to learn a size was to ask for the content.

## Decision

- **`TreeEntry.size`** is the byte size of a file (blob or executable), read from the object header without loading the content; it is absent for trees, symbolic links and submodules. An empty file has size 0, which is not absent.
- **Amends 0059 — header reads are object reads.** Each size read counts as one object of the traversal budget of the operation (no bytes are charged, none are read), so a listing stays inside the 25 000 object reads and the deadline of 0059; a tree of at most 10 000 entries (0043) cannot exhaust it alone.
- **Display rules of the web application.** A file is shown as text up to 512 KiB (highlighted up to 256 KiB); above that, or when `GetBlob` answers `BLOB_TOO_LARGE`, the page shows the size and a link to `/raw/` (0021) instead of the content. A symbolic link shows its target, a submodule the commit id it records.

## Consequences

- Additive proto change (`optional uint64 size`); the extra header reads cost one lookup per file of the listed directory.
