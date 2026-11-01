# Filesystem metadata and directory operations

The current workspace's `tinox.core.fs` adds these APIs. Published package
version documentation remains an immutable description of that version.
These APIs use POSIX filesystem operations directly; no path passes through
a shell. Existing `readFile`, `appendFile`, `copyFile`, `moveFile`, and
`listDirectory` contracts are unchanged. Use `rename` for atomic moves.

| API | Contract |
| --- | --- |
| `Fs::stat(path) -> FileInfo?` | No-follow metadata of the named entry. ENOENT and ENOTDIR return null; all other failures throw a descriptive String. A dangling symlink has metadata. Trailing slashes are removed before lookup, so they cannot cause a final symlink to be followed. Parent symlinks may be resolved by the OS. |
| `Fs::isFile/isDirectory/isSymlink(path) -> Bool` | Uses the same no-follow metadata and failure contract. Missing returns false; a symlink to a directory is a symlink, not a directory. |
| `Fs::listEntries(path) -> List<FileInfo>` | One native batch enumerates the directory and reads entry metadata with `fstatat(..., AT_SYMLINK_NOFOLLOW)`. No per-entry metadata-query transitions; wrapper conversion/object allocation still call runtime helpers. Filesystem order, excluding `.` and `..`. Listing the directory path itself follows symlinks as `opendir` does; entry targets are never followed. Any error, including a disappearing entry, throws and discards the partial result. |
| `Fs::rename(from, to)` | POSIX atomic rename within a filesystem, including directories. Keeps inode contents and permissions; existing destinations follow POSIX replacement rules. Cross-device movement throws an explicit EXDEV error; there is no copy/delete fallback. |
| `Fs::deleteDirectory(path)` | Empty-directory removal only; nonempty, missing, inaccessible, and symlink paths throw. |
| `Fs::deleteDirectoryRecursive(path)` | Removes a directory tree or the named symlink itself. Uses descriptor-relative no-follow traversal for both parents and children. Rejects symlink ancestors, regular-file roots, and final `.`/`..`/root paths. A symlink inside the tree is unlinked; its target is untouched. Concurrent modifications may cause a visible error and partial deletion. |
| `Fs::realPath(path) -> String` | Returns the absolute canonical existing path, resolving symlinks. Failures throw. |
| `Fs::homeDirectory() -> String` | Returns nonempty HOME if set; otherwise resolves the current user's home with `getpwuid_r`. Failure throws. |

`FileInfo` owns its strings and is a metadata snapshot, independent of an
open directory handle. Fields:

- `name: String`: the supplied path for `stat`, or basename for `listEntries`.
- `kind: String`: `file`, `directory`, `symlink`, or `other` (FIFO/socket/device).
- `size: Int64`: POSIX `st_size` in bytes, including the link text length for
  symlinks. A directory's value is filesystem metadata, not recursive size.
- `modifiedNanoseconds: Int64`: signed nanoseconds since the Unix epoch.
- `permissions: Int64`: POSIX mode bits masked by octal `07777`, including
  setuid/setgid/sticky bits; the file-type bits are excluded.

A timestamp outside signed Int64 nanoseconds (approximately years 1677–2262)
or an unsupported negative size raises an overflow error instead of wrapping.
Metadata is a snapshot, not a transaction: entries can change while being
listed. Recursive traversal requires readable directories and consumes one
open descriptor per depth; resource/permission failures remain visible.
Descriptor anchoring and inode checks prevent following a replaced symlink,
but concurrent directory renames can move an already-open tree; callers must
coordinate destructive mutations when that stronger isolation is required.

The nullable `stat` API follows issue #508's explicit requested signature.
Check for null, then explicitly cast to `FileInfo` before field access:

```tinox
import tinox.core.fs

const value: FileInfo? = Fs::stat("workspace")
if (value != null) {
    const info: FileInfo = value as FileInfo
    println(info.kind)
}
```

## Listing measurement

A local Linux x86-64 Release/ordinary-GC probe on a tmpfs `/tmp` directory
with 5,000 small regular files ran 20 `listEntries` batches per sample.
Three warm-cache samples took 38/35/36 ms in total (1.90/1.75/1.80 ms per
listing), measured by the Tinox millisecond clock, including metadata string
conversion and FileInfo allocation. Process wall time was 40/38/38 ms.
This is local evidence, not a cross-machine or cold-cache guarantee. The
batching guarantee is exactly one `fsMetadataChecked(path, 1)` query per
listing; the native query still performs OS metadata lookups per entry and
the wrapper uses runtime helpers for conversion and allocation.
