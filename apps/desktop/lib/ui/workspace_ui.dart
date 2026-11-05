// The folder picker and everything around it (docs/alpha-workspaces.md,
// section 4): the Folder row, the chip with its shell state, the confirm
// sheet that shows the core's verdict, the missing / denied cards, and the
// core-side folder browser for a remote core. Written against
// [WorkspaceApi] only.
import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../state/workspaces.dart';
import '../theme.dart';
import 'underline_tabs.dart';

/// The bits that touch the machine, replaceable in tests.
class WorkspaceEnv {
  /// The native directory picker. Null result = cancelled.
  ///
  /// macOS entitlements: the app runs unsandboxed (`app-sandbox` is false in
  /// both entitlements files), so the picker needs no
  /// `files.user-selected.read-write` entitlement and no security-scoped
  /// bookmark; the daemon, a separate process, opens the path itself and a
  /// TCC refusal comes back as a `denied` verdict. If the app is ever
  /// sandboxed this has to change (docs/alpha-workspaces.md section 4).
  static Future<String?> Function(String? initial) picker = (initial) =>
      getDirectoryPath(initialDirectory: initial, confirmButtonText: 'Choose');

  /// Show a path in Finder (or the file manager).
  static Future<void> Function(String path) reveal = (path) async {
    if (Platform.isMacOS) {
      await Process.run('open', ['-R', path]);
    } else if (Platform.isLinux) {
      await Process.run('xdg-open', [path]);
    }
  };

  /// Open a URL or settings pane.
  static Future<void> Function(String url) openUrl = (url) async {
    if (Platform.isMacOS) await Process.run('open', [url]);
  };

  static String get fileManager =>
      Platform.isMacOS ? 'Finder' : 'your file manager';

  static const privacyPane =
      'x-apple.systempreferences:com.apple.preference.security'
      '?Privacy_FilesAndFolders';
}

const _revealLabelKey = Key('folder-reveal');

String _revealLabel() => 'Reveal in ${WorkspaceEnv.fileManager}';

// ---------------------------------------------------------------------------
// Choosing: picker, then the core's verdict.

/// Pick a folder and have the core judge it. Returns the folder to use (the
/// core's canonical path) or null when cancelled. Local core: the native
/// picker; remote core: the core-side browser.
Future<String?> chooseFolder(
  BuildContext context,
  WorkspaceApi api, {
  String? initial,
}) async {
  if (!api.isLocal) {
    return showDialog<String>(
      context: context,
      builder: (_) => RemoteFolderBrowser(api: api, start: initial),
    );
  }
  final picked = await WorkspaceEnv.picker(initial);
  if (picked == null || !context.mounted) return null;
  return showDialog<String>(
    context: context,
    builder: (_) => ConfirmFolderDialog(api: api, path: picked),
  );
}

/// What the core says about a chosen folder, and the choice to use it.
class ConfirmFolderDialog extends StatefulWidget {
  const ConfirmFolderDialog({super.key, required this.api, required this.path});

  final WorkspaceApi api;
  final String path;

  @override
  State<ConfirmFolderDialog> createState() => _ConfirmFolderDialogState();
}

class _ConfirmFolderDialogState extends State<ConfirmFolderDialog> {
  late String _path = widget.path;
  Verdict? _verdict;
  String? _failure;

  @override
  void initState() {
    super.initState();
    _check();
  }

  Future<void> _check() async {
    setState(() {
      _verdict = null;
      _failure = null;
    });
    try {
      final v = await widget.api.validate(_path);
      if (mounted) setState(() => _verdict = v);
    } on WorkspaceRefused catch (e) {
      if (mounted) setState(() => _failure = e.message);
    } on Object {
      if (mounted) {
        setState(() => _failure = 'Tern could not check this folder.');
      }
    }
  }

  Future<void> _another() async {
    final p = await WorkspaceEnv.picker(_path);
    if (p == null || !mounted) return;
    _path = p;
    await _check();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final v = _verdict;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Use this folder?',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 10),
              _PathLine(path: v?.canonical ?? _path),
              if (v != null && v.canonical != _path) ...[
                const SizedBox(height: 6),
                Text(
                  'You chose $_path. It is a shortcut to this folder.',
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              ],
              const SizedBox(height: 12),
              if (_failure case final f?)
                _Notice(
                  key: const Key('folder-failure'),
                  icon: Icons.error_outline,
                  color: c.error,
                  text: f,
                )
              else if (v == null)
                Text('Checking...', style: TextStyle(color: c.muted))
              else ...[
                for (final e in v.errors)
                  _Notice(
                    key: const Key('folder-refusal'),
                    icon: Icons.block,
                    color: c.error,
                    text: e,
                  ),
                if (v.denied) const DeniedCopy(),
                for (final w in v.warnings)
                  _Notice(
                    key: const Key('folder-warning'),
                    icon: Icons.warning_amber_outlined,
                    color: c.warn,
                    text: w,
                  ),
                if (v.ok && v.warnings.isEmpty)
                  Text(
                    'Tern can use this folder. The agent can read and change '
                    'files in it, and nowhere else.',
                    style: TextStyle(fontSize: 13, color: c.muted),
                  ),
              ],
              const SizedBox(height: 16),
              Wrap(
                alignment: WrapAlignment.end,
                spacing: 8,
                runSpacing: 4,
                children: [
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Cancel'),
                  ),
                  if (v != null && !v.ok || _failure != null)
                    OutlinedButton(
                      key: const Key('folder-another'),
                      onPressed: _another,
                      child: const Text('Choose another...'),
                    ),
                  FilledButton(
                    key: const Key('folder-use'),
                    onPressed: v != null && v.ok
                        ? () => Navigator.pop(context, v.canonical)
                        : null,
                    child: Text(
                      v != null && v.warnings.isNotEmpty
                          ? 'Use anyway'
                          : 'Use this folder',
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _PathLine extends StatelessWidget {
  const _PathLine({required this.path});
  final String path;

  @override
  Widget build(BuildContext context) => Semantics(
    label: 'Folder $path',
    child: ExcludeSemantics(
      child: SelectableText(
        path,
        style: const TextStyle(fontFamily: 'TernMono', fontSize: 12.5),
      ),
    ),
  );
}

class _Notice extends StatelessWidget {
  const _Notice({
    super.key,
    required this.icon,
    required this.color,
    required this.text,
  });
  final IconData icon;
  final Color color;
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 6),
    child: Semantics(
      liveRegion: true,
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 2),
            child: Icon(icon, size: 15, color: color),
          ),
          const SizedBox(width: 8),
          Expanded(child: Text(text, style: const TextStyle(fontSize: 13))),
        ],
      ),
    ),
  );
}

// ---------------------------------------------------------------------------
// Cards for a folder that cannot be used.

/// The macOS refusal (TCC), in the design's words.
class DeniedCopy extends StatelessWidget {
  const DeniedCopy({super.key});

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      const _Notice(
        key: Key('folder-denied'),
        icon: Icons.lock_outline,
        color: Color(0xFFB3281D),
        text:
            "macOS didn't let Tern open this folder. Allow it in System "
            'Settings > Privacy & Security > Files and Folders.',
      ),
      if (Platform.isMacOS)
        Padding(
          padding: const EdgeInsets.only(bottom: 6),
          child: TextButton(
            key: const Key('open-privacy'),
            onPressed: () => WorkspaceEnv.openUrl(WorkspaceEnv.privacyPane),
            child: const Text('Open Privacy settings'),
          ),
        ),
    ],
  );
}

/// A folder the run cannot start in: missing (drive unplugged, deleted),
/// replaced (a different directory at the same path) or denied by macOS.
class WorkspaceStatusCard extends StatelessWidget {
  const WorkspaceStatusCard({
    super.key,
    required this.workspace,
    required this.onLocate,
    this.onUseDefault,
  });

  final WorkspaceView workspace;

  /// Locate (missing / replaced) or Choose another.
  final VoidCallback onLocate;
  final VoidCallback? onUseDefault;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final w = workspace;
    final (title, body) = switch (w.status) {
      WorkspaceStatus.missing => (
        '${w.label} is missing',
        'Tern did not find ${w.path}. If it is on a drive, plug it in. Tern '
            'will not create it again, so nothing runs here until you find '
            'it or choose another folder.',
      ),
      WorkspaceStatus.replaced => (
        '${w.label} is a different folder now',
        '${w.path} is no longer the folder you chose (it was replaced or '
            'moved). Nothing runs here until you confirm where it is.',
      ),
      WorkspaceStatus.denied => (
        'macOS blocked ${w.label}',
        "macOS didn't let Tern open this folder. Allow it in System "
            'Settings > Privacy & Security > Files and Folders, then try '
            'again.',
      ),
      WorkspaceStatus.ok => ('', ''),
    };
    return Semantics(
      container: true,
      liveRegion: true,
      label: '$title. $body',
      child: Container(
        key: Key('workspace-card-${w.status.name}'),
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          color: c.card,
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(10),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  w.status == WorkspaceStatus.denied
                      ? Icons.lock_outline
                      : Icons.folder_off_outlined,
                  size: 16,
                  color: c.error,
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    title,
                    style: const TextStyle(
                      fontSize: 13.5,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 6),
            Text(body, style: TextStyle(fontSize: 12.5, color: c.muted)),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                if (w.status == WorkspaceStatus.denied && Platform.isMacOS)
                  TextButton(
                    key: const Key('open-privacy'),
                    onPressed: () =>
                        WorkspaceEnv.openUrl(WorkspaceEnv.privacyPane),
                    child: const Text('Open Privacy settings'),
                  ),
                OutlinedButton(
                  key: const Key('card-locate'),
                  onPressed: onLocate,
                  child: Text(
                    w.status == WorkspaceStatus.denied
                        ? 'Choose another...'
                        : 'Locate...',
                  ),
                ),
                if (onUseDefault != null)
                  TextButton(
                    key: const Key('card-use-default'),
                    onPressed: onUseDefault,
                    child: const Text('Use default'),
                  ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

/// Under a header: shown only when the folder this target would use cannot
/// be used.
class WorkspaceBanner extends StatefulWidget {
  const WorkspaceBanner({
    super.key,
    required this.api,
    required this.target,
    this.revision = 0,
  });

  final WorkspaceApi api;
  final WorkspaceTarget target;

  /// Change to reload.
  final int revision;

  @override
  State<WorkspaceBanner> createState() => _WorkspaceBannerState();
}

class _WorkspaceBannerState extends State<WorkspaceBanner> {
  WorkspaceResolution? _r;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void didUpdateWidget(WorkspaceBanner old) {
    super.didUpdateWidget(old);
    if (old.target != widget.target || old.revision != widget.revision) {
      _load();
    }
  }

  Future<void> _load() async {
    try {
      final r = await widget.api.resolve(widget.target);
      if (mounted) setState(() => _r = r);
    } on Object {
      if (mounted) setState(() => _r = null);
    }
  }

  Future<void> _locate(WorkspaceResolution r) async {
    final path = await chooseFolder(
      context,
      widget.api,
      initial: r.workspace.path,
    );
    if (path == null) return;
    try {
      await widget.api.set(widget.target, path);
    } on WorkspaceRefused {
      // The sheet already showed the verdict.
    }
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final r = _r;
    if (r == null || r.workspace.status == WorkspaceStatus.ok) {
      return const SizedBox.shrink();
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(20, 8, 20, 4),
      child: WorkspaceStatusCard(
        workspace: r.workspace,
        onLocate: () => _locate(r),
        onUseDefault: r.origin == WorkspaceOrigin.global
            ? null
            : () async {
                await widget.api.clear(widget.target);
                await _load();
              },
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// The Folder row.

/// The Folder row: the path, or the default, with Choose..., Reveal and Use default.
/// Controlled: [path] is what is set (null = inherit), [onChosen] gets the
/// confirmed folder, [onClear] removes the setting.
class FolderRow extends StatefulWidget {
  const FolderRow({
    super.key,
    required this.api,
    required this.path,
    required this.onChosen,
    this.onClear,
    this.label = 'Folder',
    this.emptyLabel = 'Default folder',
    this.note,
  });

  final WorkspaceApi api;
  final String? path;
  final Future<String?> Function(String path) onChosen;
  final VoidCallback? onClear;
  final String label;

  /// Shown when [path] is null.
  final String emptyLabel;
  final String? note;

  @override
  State<FolderRow> createState() => _FolderRowState();
}

class _FolderRowState extends State<FolderRow> {
  String? _error;

  Future<void> _choose() async {
    final p = await chooseFolder(context, widget.api, initial: widget.path);
    if (p == null) return;
    final err = await widget.onChosen(p);
    if (mounted) setState(() => _error = err);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final path = widget.path;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(widget.label, style: labels),
        const SizedBox(height: 6),
        Row(
          children: [
            Icon(Icons.folder_outlined, size: 16, color: c.muted),
            const SizedBox(width: 8),
            Expanded(
              child: Semantics(
                label: '${widget.label}: ${path ?? widget.emptyLabel}',
                child: ExcludeSemantics(
                  child: Text(
                    path ?? widget.emptyLabel,
                    key: const Key('folder-value'),
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontFamily: path == null ? null : 'TernMono',
                      fontSize: 12.5,
                      color: path == null ? c.muted : c.text,
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Wrap(
          spacing: 4,
          children: [
            OutlinedButton(
              key: const Key('folder-choose'),
              onPressed: _choose,
              child: Text(widget.api.isLocal ? 'Choose...' : 'Browse...'),
            ),
            if (path != null && widget.api.isLocal)
              TextButton(
                key: _revealLabelKey,
                onPressed: () => WorkspaceEnv.reveal(path),
                child: Text(_revealLabel()),
              ),
            if (path != null && widget.onClear != null)
              TextButton(
                key: const Key('folder-use-default'),
                onPressed: () {
                  setState(() => _error = null);
                  widget.onClear!();
                },
                child: const Text('Use default'),
              ),
          ],
        ),
        if (widget.note case final n?)
          Text(n, style: TextStyle(fontSize: 12, color: c.muted)),
        if (_error case final e?)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              e,
              key: const Key('folder-error'),
              style: TextStyle(fontSize: 12.5, color: c.error),
            ),
          ),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// The chip and its detail.

IconData _shellIcon(ShellBound s) => switch (s) {
  ShellBound.bounded => Icons.shield_outlined,
  ShellBound.unverified => Icons.inventory_2_outlined,
  ShellBound.unbounded => Icons.warning_amber_outlined,
};

Color _shellColor(TernColors c, ShellBound s) => switch (s) {
  ShellBound.bounded => c.ok,
  ShellBound.unverified => c.muted,
  ShellBound.unbounded => c.warn,
};

/// The folder in a header: its name and the shell state. Click for the
/// detail. On a Home topic the change starts a new topic.
class WorkspaceChip extends StatefulWidget {
  const WorkspaceChip({
    super.key,
    required this.api,
    required this.target,
    this.homeTopic = false,
    this.onChangeHome,
    this.pending,
    this.revision = 0,
  });

  final WorkspaceApi api;
  final WorkspaceTarget target;

  /// Home: changing the folder starts a new topic ([onChangeHome]).
  final bool homeTopic;
  final void Function(String path)? onChangeHome;

  /// A folder chosen for the next, not yet started, topic.
  final String? pending;
  final int revision;

  @override
  State<WorkspaceChip> createState() => _WorkspaceChipState();
}

class _WorkspaceChipState extends State<WorkspaceChip> {
  WorkspaceResolution? _r;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void didUpdateWidget(WorkspaceChip old) {
    super.didUpdateWidget(old);
    if (old.target != widget.target || old.revision != widget.revision) {
      _load();
    }
  }

  Future<void> _load() async {
    try {
      final r = await widget.api.resolve(widget.target);
      if (mounted) setState(() => _r = r);
    } on Object {
      if (mounted) setState(() => _r = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final r = _r;
    if (r == null) return const SizedBox.shrink();
    final c = context.colors;
    final w = r.workspace;
    final broken = w.status != WorkspaceStatus.ok;
    final copy = ShellCopy.of(w.shellBound, w.label);
    final name = widget.pending != null
        ? widget.pending!.split('/').where((s) => s.isNotEmpty).lastOrNull ??
              widget.pending!
        : w.label;
    final color = broken ? c.error : _shellColor(c, w.shellBound);
    final summary = broken
        ? 'Folder $name, unavailable'
        : 'Folder $name, ${copy.headline}';
    return Semantics(
      button: true,
      label: summary,
      excludeSemantics: true,
      onTap: () => _open(r),
      child: Tooltip(
        message: broken ? summary : '${w.path}\n${copy.headline}',
        child: TextButton.icon(
          key: const Key('workspace-chip'),
          onPressed: () => _open(r),
          icon: Icon(
            broken ? Icons.folder_off_outlined : Icons.folder_outlined,
            size: 15,
            color: c.muted,
          ),
          label: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 190),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Flexible(
                  child: Text(
                    name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 12.5, color: c.muted),
                  ),
                ),
                const SizedBox(width: 6),
                Icon(
                  broken ? Icons.error_outline : _shellIcon(w.shellBound),
                  key: const Key('chip-shell-icon'),
                  size: 14,
                  color: color,
                ),
              ],
            ),
          ),
          style: TextButton.styleFrom(
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(8),
            ),
          ),
        ),
      ),
    );
  }

  Future<void> _open(WorkspaceResolution r) async {
    await showDialog<void>(
      context: context,
      builder: (_) => WorkspaceDetailDialog(
        api: widget.api,
        target: widget.target,
        homeTopic: widget.homeTopic,
        onChangeHome: widget.onChangeHome,
      ),
    );
    await _load();
  }
}

/// The folder, where it comes from, what is and is not confined, the
/// read-only extra folders, and the ways to change it.
class WorkspaceDetailDialog extends StatefulWidget {
  const WorkspaceDetailDialog({
    super.key,
    required this.api,
    required this.target,
    this.homeTopic = false,
    this.onChangeHome,
  });

  final WorkspaceApi api;
  final WorkspaceTarget target;
  final bool homeTopic;
  final void Function(String path)? onChangeHome;

  @override
  State<WorkspaceDetailDialog> createState() => _WorkspaceDetailDialogState();
}

class _WorkspaceDetailDialogState extends State<WorkspaceDetailDialog> {
  WorkspaceResolution? _r;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final r = await widget.api.resolve(widget.target);
      if (mounted) setState(() => _r = r);
    } on Object {
      if (mounted) setState(() => _error = 'Tern could not read the folder.');
    }
  }

  Future<void> _change() async {
    final r = _r!;
    final p = await chooseFolder(
      context,
      widget.api,
      initial: r.workspace.path,
    );
    if (p == null || !mounted) return;
    if (widget.homeTopic) {
      widget.onChangeHome?.call(p);
      if (mounted) Navigator.pop(context);
      return;
    }
    try {
      await widget.api.set(widget.target, p);
      if (mounted) setState(() => _error = null);
    } on WorkspaceRefused catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
    await _load();
  }

  Future<void> _addRoot() async {
    final r = _r!;
    final p = await chooseFolder(context, widget.api);
    if (p == null) return;
    try {
      await widget.api.addRoot(r.workspace.id, p);
      _error = null;
    } on WorkspaceRefused catch (e) {
      _error = e.message;
    }
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final r = _r;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 540),
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: r == null
              ? Text(
                  _error ?? 'Loading...',
                  style: TextStyle(color: _error == null ? c.muted : c.error),
                )
              : _body(context, r),
        ),
      ),
    );
  }

  Widget _body(BuildContext context, WorkspaceResolution r) {
    final c = context.colors;
    final w = r.workspace;
    final copy = ShellCopy.of(w.shellBound, w.label);
    final own = switch (widget.target.kind) {
      WorkspaceKind.session => r.origin == WorkspaceOrigin.session,
      WorkspaceKind.channel => r.origin == WorkspaceOrigin.channel,
      WorkspaceKind.agent => r.origin == WorkspaceOrigin.agent,
    };
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          'Folder',
          style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 10),
        _PathLine(path: w.path),
        const SizedBox(height: 4),
        Text(
          originCopy(r.origin)[0].toUpperCase() +
              originCopy(r.origin).substring(1),
          key: const Key('folder-origin'),
          style: TextStyle(fontSize: 12.5, color: c.muted),
        ),
        if (w.status != WorkspaceStatus.ok) ...[
          const SizedBox(height: 12),
          WorkspaceStatusCard(
            workspace: w,
            onLocate: _change,
            onUseDefault: own
                ? () async {
                    await widget.api.clear(widget.target);
                    await _load();
                  }
                : null,
          ),
        ],
        const SizedBox(height: 14),
        Row(
          children: [
            Icon(
              _shellIcon(w.shellBound),
              size: 16,
              color: _shellColor(c, w.shellBound),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                copy.headline,
                key: const Key('shell-headline'),
                style: const TextStyle(
                  fontSize: 13.5,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Text(
          copy.detail,
          key: const Key('shell-detail'),
          style: const TextStyle(fontSize: 13),
        ),
        const SizedBox(height: 8),
        Text(
          'What is confined to this folder',
          style: Theme.of(context).textTheme.labelSmall,
        ),
        const SizedBox(height: 4),
        for (final line in copy.confined)
          Padding(
            padding: const EdgeInsets.only(bottom: 3),
            child: Text(line, style: TextStyle(fontSize: 12.5, color: c.muted)),
          ),
        if (w.shellBound == ShellBound.unbounded)
          Padding(
            padding: const EdgeInsets.only(top: 2),
            child: Text(
              'Run tern doctor for why commands are not sandboxed here.',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
        const SizedBox(height: 14),
        Text(
          'Extra folders (read only)',
          style: Theme.of(context).textTheme.labelSmall,
        ),
        const SizedBox(height: 4),
        if (w.extraRoots.isEmpty)
          Text(
            'None. The agent can read only this folder.',
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
        for (final root in w.extraRoots)
          _RootRow(
            path: root,
            onRemove: () async {
              await widget.api.removeRoot(w.id, root);
              await _load();
            },
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const Key('add-root'),
            onPressed: _addRoot,
            child: const Text('Add a read-only folder...'),
          ),
        ),
        if (_error case final e?)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              e,
              key: const Key('folder-error'),
              style: TextStyle(fontSize: 12.5, color: c.error),
            ),
          ),
        const SizedBox(height: 12),
        Wrap(
          alignment: WrapAlignment.end,
          spacing: 8,
          runSpacing: 4,
          children: [
            if (widget.api.isLocal)
              TextButton(
                key: _revealLabelKey,
                onPressed: () => WorkspaceEnv.reveal(w.path),
                child: Text(_revealLabel()),
              ),
            if (own && !widget.homeTopic)
              TextButton(
                key: const Key('folder-use-default'),
                onPressed: () async {
                  await widget.api.clear(widget.target);
                  await _load();
                },
                child: const Text('Use default'),
              ),
            OutlinedButton(
              key: const Key('folder-change'),
              onPressed: _change,
              child: Text(
                widget.homeTopic
                    ? 'Change folder (starts a new topic)'
                    : 'Change...',
              ),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Done'),
            ),
          ],
        ),
      ],
    );
  }
}

class _RootRow extends StatelessWidget {
  const _RootRow({required this.path, required this.onRemove});
  final String path;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) => Row(
    children: [
      Expanded(
        child: Text(
          path,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: const TextStyle(fontFamily: 'TernMono', fontSize: 12.5),
        ),
      ),
      IconButton(
        tooltip: 'Remove $path',
        icon: const Icon(Icons.close, size: 16),
        visualDensity: VisualDensity.compact,
        onPressed: onRemove,
      ),
    ],
  );
}

// ---------------------------------------------------------------------------
// Remote core: browse the core's own folders.

/// The core-side folder browser: directories only, below the core's home,
/// with the same verdict preview as a local choice.
class RemoteFolderBrowser extends StatefulWidget {
  const RemoteFolderBrowser({super.key, required this.api, this.start});

  final WorkspaceApi api;
  final String? start;

  @override
  State<RemoteFolderBrowser> createState() => _RemoteFolderBrowserState();
}

class _RemoteFolderBrowserState extends State<RemoteFolderBrowser> {
  BrowseListing? _listing;
  Verdict? _verdict;
  String? _error;

  @override
  void initState() {
    super.initState();
    _go(widget.start);
  }

  Future<void> _go(String? path) async {
    setState(() {
      _error = null;
      _verdict = null;
    });
    try {
      final l = await widget.api.browse(path);
      if (!mounted) return;
      setState(() => _listing = l);
      final v = await widget.api.validate(l.path);
      if (mounted) setState(() => _verdict = v);
    } on WorkspaceRefused catch (e) {
      if (mounted) setState(() => _error = e.message);
    } on Object {
      if (mounted) setState(() => _error = 'Could not list that folder.');
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final l = _listing;
    final v = _verdict;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520, maxHeight: 560),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Choose a folder on the server',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 4),
              Text(
                'This is a folder on the computer tern runs on, not on this one.',
                style: TextStyle(fontSize: 12.5, color: c.muted),
              ),
              const SizedBox(height: 10),
              if (l != null) _PathLine(path: l.path),
              const SizedBox(height: 6),
              Flexible(
                child: _error != null
                    ? Text(_error!, style: TextStyle(color: c.error))
                    : l == null
                    ? Text('Loading...', style: TextStyle(color: c.muted))
                    : ListView(
                        shrinkWrap: true,
                        children: [
                          if (l.parent case final p?)
                            ListTile(
                              key: const Key('browse-up'),
                              dense: true,
                              leading: const Icon(Icons.arrow_upward, size: 18),
                              title: const Text('Up one level'),
                              onTap: () => _go(p),
                            ),
                          for (final d in l.dirs)
                            ListTile(
                              key: Key('browse-$d'),
                              dense: true,
                              leading: const Icon(
                                Icons.folder_outlined,
                                size: 18,
                              ),
                              title: Text(d),
                              onTap: () => _go('${l.path}/$d'),
                            ),
                          if (l.dirs.isEmpty)
                            Padding(
                              padding: const EdgeInsets.all(8),
                              child: Text(
                                'No folders here.',
                                style: TextStyle(color: c.muted),
                              ),
                            ),
                        ],
                      ),
              ),
              if (v != null) ...[
                const SizedBox(height: 6),
                for (final e in v.errors)
                  _Notice(
                    key: const Key('folder-refusal'),
                    icon: Icons.block,
                    color: c.error,
                    text: e,
                  ),
                if (v.denied) const DeniedCopy(),
                for (final w in v.warnings)
                  _Notice(
                    icon: Icons.warning_amber_outlined,
                    color: c.warn,
                    text: w,
                  ),
              ],
              const SizedBox(height: 12),
              Wrap(
                alignment: WrapAlignment.end,
                spacing: 8,
                children: [
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Cancel'),
                  ),
                  FilledButton(
                    key: const Key('folder-use'),
                    onPressed: v != null && v.ok
                        ? () => Navigator.pop(context, v.canonical)
                        : null,
                    child: const Text('Use this folder'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// Settings > Folders.

enum _Filter { all, channels, agents, topics }

/// The Folders section of the Settings window: the default folder, what has
/// its own folder, and read-only extra folders.
class FoldersSection extends StatefulWidget {
  const FoldersSection({super.key, required this.api});

  final WorkspaceApi api;

  @override
  State<FoldersSection> createState() => _FoldersSectionState();
}

class _FoldersSectionState extends State<FoldersSection> {
  WorkspaceView? _default;
  List<WorkspaceView> _all = const [];
  var _filter = _Filter.all;
  String? _error;
  var _loaded = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final d = await widget.api.getDefault();
      final all = await widget.api.list();
      if (mounted) {
        setState(() {
          _default = d;
          _all = all;
          _loaded = true;
        });
      }
    } on Object {
      if (mounted) {
        setState(() {
          _error = 'Tern could not read the folders.';
          _loaded = true;
        });
      }
    }
  }

  Future<void> _run(Future<void> Function() f) async {
    try {
      await f();
      _error = null;
    } on WorkspaceRefused catch (e) {
      _error = e.message;
    }
    await _load();
  }

  bool _keep(WorkspaceUse u) => switch (_filter) {
    _Filter.all => true,
    _Filter.channels => u.kind == WorkspaceKind.channel,
    _Filter.agents => u.kind == WorkspaceKind.agent,
    _Filter.topics => u.kind == WorkspaceKind.session,
  };

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final d = _default;
    if (!_loaded) {
      return Text('Loading...', style: TextStyle(color: c.muted));
    }
    final overrides = [
      for (final w in _all)
        for (final u in w.usedBy)
          if (_keep(u)) (w, u),
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (d != null) ...[
          FolderRow(
            api: widget.api,
            label: 'Default folder',
            path: d.path,
            note:
                'Used by every topic, channel and agent that has not '
                'chosen its own.',
            onChosen: (p) async {
              try {
                await widget.api.setDefault(p);
                await _load();
                return null;
              } on WorkspaceRefused catch (e) {
                return e.message;
              }
            },
          ),
          if (d.status != WorkspaceStatus.ok) ...[
            const SizedBox(height: 8),
            WorkspaceStatusCard(
              workspace: d,
              onLocate: () async {
                final p = await chooseFolder(
                  context,
                  widget.api,
                  initial: d.path,
                );
                if (p != null) {
                  await _run(() => widget.api.setDefault(p).then((_) {}));
                }
              },
            ),
          ],
          const SizedBox(height: 8),
          Text(
            ShellCopy.of(d.shellBound, d.label).detail,
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
        ],
        const SizedBox(height: 20),
        Text('Own folders', style: labels),
        const SizedBox(height: 4),
        UnderlineTabs(
          tabs: [
            for (final f in _Filter.values)
              UnderlineTab(
                key: Key('folders-tab-${f.name}'),
                label: switch (f) {
                  _Filter.all => 'All',
                  _Filter.channels => 'Channels',
                  _Filter.agents => 'Agents',
                  _Filter.topics => 'Topics',
                },
                selected: _filter == f,
                onTap: () => setState(() => _filter = f),
              ),
          ],
        ),
        const SizedBox(height: 6),
        if (overrides.isEmpty)
          Text(
            'Nothing here has its own folder.',
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
        for (final (w, u) in overrides)
          _OverrideRow(
            key: Key('override-${u.kind.name}-${u.id}'),
            workspace: w,
            use: u,
            onUseDefault: () =>
                _run(() => widget.api.clear(u.target).then((_) {})),
          ),
        const SizedBox(height: 20),
        Text('Read-only extra folders', style: labels),
        const SizedBox(height: 4),
        Text(
          'Folders the agent may read but not change, added to a folder.',
          style: TextStyle(fontSize: 12.5, color: c.muted),
        ),
        for (final w in _all)
          _RootsGroup(
            key: Key('roots-${w.id}'),
            workspace: w,
            onAdd: () async {
              final p = await chooseFolder(context, widget.api);
              if (p != null) {
                await _run(() => widget.api.addRoot(w.id, p).then((_) {}));
              }
            },
            onRemove: (r) =>
                _run(() => widget.api.removeRoot(w.id, r).then((_) {})),
          ),
        if (_error case final e?)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(
              e,
              key: const Key('folder-error'),
              style: TextStyle(fontSize: 12.5, color: c.error),
            ),
          ),
      ],
    );
  }
}

/// An agent's own default folder, saved as soon as it is chosen (the agent
/// exists already). Without one the agent uses your default folder.
class AgentFolderField extends StatefulWidget {
  const AgentFolderField({super.key, required this.api, required this.agentId});

  final WorkspaceApi api;
  final String agentId;

  @override
  State<AgentFolderField> createState() => _AgentFolderFieldState();
}

class _AgentFolderFieldState extends State<AgentFolderField> {
  WorkspaceResolution? _r;

  WorkspaceTarget get _t => WorkspaceTarget.agent(widget.agentId);

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final r = await widget.api.resolve(_t);
      if (mounted) setState(() => _r = r);
    } on Object {
      if (mounted) setState(() => _r = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final r = _r;
    if (r == null) return const SizedBox.shrink();
    final own = r.origin == WorkspaceOrigin.agent;
    return FolderRow(
      api: widget.api,
      label: 'Default folder',
      path: own ? r.workspace.path : null,
      emptyLabel: 'Your default folder (${r.workspace.path})',
      note:
          'Where this agent works unless a channel or topic picks another '
          'folder.',
      onChosen: (p) async {
        try {
          await widget.api.set(_t, p);
          await _load();
          return null;
        } on WorkspaceRefused catch (e) {
          return e.message;
        }
      },
      onClear: () async {
        await widget.api.clear(_t);
        await _load();
      },
    );
  }
}

class _OverrideRow extends StatelessWidget {
  const _OverrideRow({
    super.key,
    required this.workspace,
    required this.use,
    required this.onUseDefault,
  });

  final WorkspaceView workspace;
  final WorkspaceUse use;
  final VoidCallback onUseDefault;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final broken = workspace.status != WorkspaceStatus.ok;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        children: [
          Icon(
            broken ? Icons.folder_off_outlined : Icons.folder_outlined,
            size: 16,
            color: broken ? c.error : c.muted,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(use.label, style: const TextStyle(fontSize: 13)),
                Text(
                  broken
                      ? '${workspace.path} (${workspace.status.name})'
                      : workspace.path,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    fontFamily: 'TernMono',
                    fontSize: 11.5,
                    color: broken ? c.error : c.muted,
                  ),
                ),
              ],
            ),
          ),
          TextButton(
            key: const Key('override-use-default'),
            onPressed: onUseDefault,
            child: const Text('Use default'),
          ),
        ],
      ),
    );
  }
}

class _RootsGroup extends StatelessWidget {
  const _RootsGroup({
    super.key,
    required this.workspace,
    required this.onAdd,
    required this.onRemove,
  });

  final WorkspaceView workspace;
  final VoidCallback onAdd;
  final void Function(String root) onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.only(top: 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            workspace.label,
            style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600),
          ),
          for (final r in workspace.extraRoots)
            _RootRow(path: r, onRemove: () => onRemove(r)),
          if (workspace.extraRoots.isEmpty)
            Text('None', style: TextStyle(fontSize: 12.5, color: c.muted)),
          TextButton(
            key: Key('add-root-${workspace.id}'),
            onPressed: onAdd,
            child: Text('Add a read-only folder to ${workspace.label}...'),
          ),
        ],
      ),
    );
  }
}
