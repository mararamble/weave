# Executed from stdin by python3 -I -B; never installed on the remote host.
import json
import os
import re
import stat
import sys

MAX_FILE = 1024 * 1024
MAX_DIRS = 10000
MAX_RECORDS = 1000
MAX_ISSUES = 1000


def probe(request):
    roots = [os.fsencode(p) for p in request['roots']]
    records, issues, seen = [], [], set()
    visited = 0

    def issue(path, reason):
        if len(issues) < MAX_ISSUES:
            issues.append(dict(path_hex=path.hex(), reason=reason))

    def permitted(path):
        return any(path == r or path.startswith(r + b'/') for r in roots)

    def normalized(path):
        path = os.path.normpath(path)
        if not path.startswith(b'/') or not permitted(path):
            raise ValueError('metadata_outside_roots')
        return path

    def opened(path, directory=False):
        # Walk every component with O_NOFOLLOW, including ancestors of roots.
        path = normalized(path)
        fd = os.open(b'/', os.O_RDONLY | os.O_DIRECTORY)
        try:
            parts = path.split(b'/')[1:]
            for i, part in enumerate(parts):
                flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
                if i < len(parts) - 1 or directory:
                    flags |= os.O_DIRECTORY
                other = os.open(part, flags, dir_fd=fd)
                os.close(fd)
                fd = other
            result = fd
            fd = None
            return result
        finally:
            if fd is not None:
                os.close(fd)

    def read(path):
        fd = opened(path)
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                raise ValueError('unsupported_metadata_file')
            chunks = bytearray()
            while len(chunks) <= MAX_FILE:
                chunk = os.read(fd, min(65536, MAX_FILE + 1 - len(chunks)))
                if not chunk:
                    return bytes(chunks)
                chunks.extend(chunk)
            raise ValueError('metadata_size_limit')
        finally:
            os.close(fd)

    def names(path):
        fd = opened(path, True)
        try:
            with os.scandir(fd) as entries:
                # Bound directory enumeration as well as traversal.
                result = []
                for entry in entries:
                    if len(result) >= MAX_DIRS:
                        raise ValueError('directory_entry_limit')
                    result.append(os.fsencode(entry.name))
                return sorted(result)
        finally:
            os.close(fd)

    def is_link(path):
        parent, name = os.path.split(path)
        fd = opened(parent, True)
        try:
            return stat.S_ISLNK(os.stat(name, dir_fd=fd, follow_symlinks=False).st_mode)
        finally:
            os.close(fd)

    def optional(path):
        try:
            return read(path)
        except FileNotFoundError:
            return None

    def resolve(common, ref, depth=0):
        if (depth > 8 or not ref.startswith(b'refs/') or
                re.search(rb'[\x00-\x20\x7f~^:?*\[\\]', ref) or
                any(p.startswith(b'.') or p.endswith((b'.', b'.lock')) or not p for p in ref.split(b'/')) or
                b'..' in ref or b'@{' in ref):
            raise ValueError('unsupported_ref')
        data = optional(common + b'/' + ref)
        if data is not None:
            data = data.rstrip(b'\n')
            if data.startswith(b'ref: '):
                return resolve(common, data[5:], depth + 1)
            if not re.fullmatch(rb'[0-9a-f]{40}|[0-9a-f]{64}', data):
                raise ValueError('malformed_ref')
            return data.decode('ascii')
        packed = optional(common + b'/packed-refs')
        if packed:
            for line in packed.splitlines():
                if line.startswith((b'#', b'^')):
                    continue
                fields = line.split(b' ')
                if len(fields) != 2 or not re.fullmatch(rb'[0-9a-f]{40}|[0-9a-f]{64}', fields[0]):
                    raise ValueError('malformed_packed_refs')
                if fields[1] == ref:
                    return fields[0].decode('ascii')
        return None

    def record(path, gitdir, kind, registrations=True):
        path, gitdir = normalized(path), normalized(gitdir)
        if path in seen:
            return
        if len(records) >= MAX_RECORDS:
            raise ValueError('record_limit')
        common = optional(gitdir + b'/commondir')
        if common is not None:
            common = common.removesuffix(b'\n')
            common = normalized(os.path.join(gitdir, common))
        else:
            common = gitdir
        # Refuse reftable and extensions we cannot faithfully interpret.
        if b'reftable' in names(common):
            raise ValueError('unsupported_reftable')
        head = read(gitdir + b'/HEAD').removesuffix(b'\n')
        branch = None
        if head.startswith(b'ref: '):
            ref = head[5:]
            if not ref.startswith(b'refs/heads/'):
                raise ValueError('unsupported_head_ref')
            branch = ref[11:].decode('utf-8', 'strict')
            oid = resolve(common, ref)
        elif re.fullmatch(rb'[0-9a-f]{40}|[0-9a-f]{64}', head):
            oid = head.decode('ascii')
        else:
            raise ValueError('malformed_head')
        seen.add(path)
        records.append(dict(path=path.decode('utf-8', 'replace'), path_hex=path.hex(),
                            paths_lossy=path.decode('utf-8', 'replace').encode('utf-8') != path,
                            git_dir_hex=gitdir.hex(), common_dir_hex=common.hex(),
                            kind='linked' if common != gitdir else kind, branch=branch, head=oid, unborn=oid is None))
        if not registrations:
            return
        # Enumerate registrations, including worktrees beyond traversal depth.
        try:
            registrations = names(common + b'/worktrees')
        except FileNotFoundError:
            registrations = []
        for name in registrations:
            registration = common + b'/worktrees/' + name
            try:
                pointer = read(registration + b'/gitdir').removesuffix(b'\n')
                target = normalized(os.path.dirname(pointer))
                # Verify both directions rather than trusting stale registrations.
                if read(target + b'/.git').removesuffix(b'\n') != b'gitdir: ' + registration:
                    raise ValueError('inconsistent_worktree_registration')
                record(target, registration, 'linked', False)
            except (OSError, ValueError, UnicodeError) as error:
                issue(registration, reason(error))

    def reason(error):
        if isinstance(error, PermissionError):
            return 'permission_denied'
        if isinstance(error, FileNotFoundError):
            return 'missing_path'
        if isinstance(error, UnicodeError):
            return 'unsupported_metadata_encoding'
        if isinstance(error, OSError):
            return 'symlink_or_io_error'
        return str(error)

    def walk(path, depth):
        nonlocal visited
        if visited >= MAX_DIRS:
            issue(path, 'directory_limit')
            return
        visited += 1
        try:
            entries = names(path)
            if b'.git' in entries:
                marker = path + b'/.git'
                try:
                    fd = opened(marker, True)
                except NotADirectoryError:
                    pointer = read(marker).removesuffix(b'\n')
                    if not pointer.startswith(b'gitdir: '):
                        raise ValueError('malformed_gitdir')
                    gitdir = normalized(os.path.join(path, pointer[8:]))
                else:
                    os.close(fd)
                    gitdir = marker
                record(path, gitdir, 'worktree')
            elif b'HEAD' in entries and b'objects' in entries and b'refs' in entries:
                record(path, path, 'bare')
                return
            for name in entries:
                if name == b'.git':
                    continue
                child = path + b'/' + name
                try:
                    fd = opened(child, True)
                    os.close(fd)
                except NotADirectoryError:
                    if is_link(child):
                        issue(child, 'symlink_skipped')
                    continue
                except OSError as error:
                    issue(child, reason(error))
                    continue
                if depth >= request['max_depth']:
                    issue(child, 'depth_limit')
                elif len(records) >= MAX_RECORDS or visited >= MAX_DIRS:
                    issue(child, 'traversal_limit')
                    break
                else:
                    walk(child, depth + 1)
        except (OSError, ValueError, UnicodeError) as error:
            issue(path, reason(error))

    if request['operation'] == 'discover':
        for root in roots:
            walk(root, 0)
    return dict(schema_version=1, state='partial' if issues else 'complete',
                repositories=records, issues=issues)


if __name__ == '__main__':
    result = probe(REQUEST)
    payload = json.dumps(result, ensure_ascii=True, separators=(',', ':'))
    # The per-request unpredictable token separates shell banners from protocol.
    sys.stdout.write('\nWEAVE_REMOTE_' + REQUEST['token'] + '_BEGIN\n' + payload +
                     '\nWEAVE_REMOTE_' + REQUEST['token'] + '_END\n')
