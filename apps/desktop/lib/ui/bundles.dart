import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// Library → Bundles (tooling spec §6.5): connectors, skills and routines
/// someone shared as a URL or a file. Installing shows everything a bundle
/// can do first; connectors arrive inactive and routines paused. There is
/// no store to browse, on purpose. Owner-only.
class BundlesView extends StatelessWidget {
  const BundlesView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) => Column(
    children: [
      WindowDragArea(
        child: Container(
          height: titleBarInset + 12,
          padding: const EdgeInsets.fromLTRB(12, 0, 10, 0),
          child: Row(
            children: [
              const SizedBox(width: 12),
              const Expanded(
                child: Text(
                  'Bundles',
                  style: TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
                ),
              ),
              TextButton.icon(
                icon: const Icon(Icons.add, size: 16),
                label: const Text('Install a bundle…'),
                onPressed: () => showInstallBundle(context, store),
              ),
              QuietIconButton(
                icon: panelOpen
                    ? Icons.view_sidebar
                    : Icons.view_sidebar_outlined,
                tooltip: panelOpen ? 'Hide activity' : 'Show activity',
                onPressed: onTogglePanel,
              ),
            ],
          ),
        ),
      ),
      const Hairline(),
      Expanded(child: _BundleList(store: store)),
    ],
  );
}

class _BundleList extends StatelessWidget {
  const _BundleList({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final list = store.bundles;
    if (list == null) {
      return const Center(child: CircularProgressIndicator(strokeWidth: 2));
    }
    if (list.isEmpty) {
      return Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Text(
            'No bundles yet. A bundle is a set of connectors, skills and '
            'routines someone shared as a link or a file. You see everything '
            'it can do before anything is installed.\n\n'
            'Make your own with `tern bundle export`.',
            textAlign: TextAlign.center,
            style: TextStyle(color: c.muted, height: 1.4),
          ),
        ),
      );
    }
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 16, 24, 24),
      children: [for (final b in list) _BundleTile(store: store, bundle: b)],
    );
  }
}

class _BundleTile extends StatelessWidget {
  const _BundleTile({required this.store, required this.bundle});

  final TernStore store;
  final InstalledBundle bundle;

  Future<void> _remove(BuildContext context) async {
    final b = bundle;
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${b.name}?'),
        content: Text(
          'This removes what it installed: '
          '${[if (b.connectors.isNotEmpty) 'connectors ${b.connectors.join(', ')}', if (b.skills.isNotEmpty) 'skills ${b.skills.join(', ')}', if (b.routines.isNotEmpty) '${b.routines.length} routine${b.routines.length == 1 ? '' : 's'}'].join('; ')}. '
          'A connector you changed since is kept.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    final err = await store.removeBundle(b.name);
    if (err != null && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(err)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final b = bundle;
    final contents = [
      if (b.connectors.isNotEmpty) 'Connectors: ${b.connectors.join(', ')}',
      if (b.skills.isNotEmpty) 'Skills: ${b.skills.join(', ')}',
      if (b.routines.isNotEmpty) 'Routines: ${b.routines.join(', ')}',
    ];
    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      padding: const EdgeInsets.all(14),
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
              Expanded(
                child: Text(
                  '${b.name} ${b.version}',
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
              ),
              if (!b.imported)
                TextButton(
                  onPressed: () =>
                      showInstallBundle(context, store, source: b.source),
                  child: const Text('Check for update'),
                ),
              TextButton(
                onPressed: () => _remove(context),
                child: const Text('Remove'),
              ),
            ],
          ),
          const SizedBox(height: 4),
          _SignatureLine(
            signed: b.signed,
            trusted: b.keyTrusted,
            words: b.signature,
          ),
          const SizedBox(height: 4),
          Text(
            b.source,
            style: TextStyle(fontSize: 12, color: c.muted),
            overflow: TextOverflow.ellipsis,
          ),
          for (final line in contents)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(line, style: const TextStyle(fontSize: 13)),
            ),
        ],
      ),
    );
  }
}

class _SignatureLine extends StatelessWidget {
  const _SignatureLine({
    required this.signed,
    required this.trusted,
    required this.words,
  });

  final bool signed;
  final bool trusted;
  final String words;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final color = !signed
        ? c.warn
        : trusted
        ? c.ok
        : c.muted;
    return Row(
      children: [
        Icon(
          signed ? Icons.verified_outlined : Icons.warning_amber_rounded,
          size: 14,
          color: color,
        ),
        const SizedBox(width: 6),
        Flexible(
          child: Text(words, style: TextStyle(fontSize: 12.5, color: color)),
        ),
      ],
    );
  }
}

/// Ask for a bundle's address, show everything it can do, and install it
/// on approval. With `source`, start from that address (an update).
Future<void> showInstallBundle(
  BuildContext context,
  TernStore store, {
  String? source,
}) => showDialog<void>(
  context: context,
  builder: (_) => _InstallDialog(store: store, source: source),
);

class _InstallDialog extends StatefulWidget {
  const _InstallDialog({required this.store, this.source});

  final TernStore store;
  final String? source;

  @override
  State<_InstallDialog> createState() => _InstallDialogState();
}

class _InstallDialogState extends State<_InstallDialog> {
  late final _source = TextEditingController(text: widget.source ?? '');
  final _sha = TextEditingController();
  BundlePreview? _preview;
  bool _busy = false;
  bool _acceptKey = false;
  String? _error;
  bool _done = false;

  @override
  void initState() {
    super.initState();
    if (widget.source != null) _review();
  }

  @override
  void dispose() {
    _source.dispose();
    _sha.dispose();
    super.dispose();
  }

  Future<void> _review() async {
    final source = _source.text.trim();
    if (source.isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final sha = _sha.text.trim();
      final p = await widget.store.client.previewBundle(
        source,
        sha256: sha.isEmpty ? null : sha,
      );
      if (mounted) setState(() => _preview = p);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _install() async {
    final p = _preview!;
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.installBundle(
      _source.text.trim(),
      p.plan,
      acceptNewKey: _acceptKey,
    );
    if (!mounted) return;
    setState(() {
      _busy = false;
      _error = err;
      _done = err == null;
    });
  }

  @override
  Widget build(BuildContext context) {
    final p = _preview;
    final plan = p?.plan;
    final newKey = plan?.signatureState == 'new_key';
    final upToDate = plan?.upToDate ?? false;
    final title = _done
        ? '${plan!.name} ${plan.isUpdate ? 'updated' : 'installed'}'
        : plan == null
        ? 'Install a bundle'
        : upToDate
        ? '${plan.name} ${plan.version} is up to date'
        : '${plan.isUpdate ? 'Update' : 'Install'} ${plan.name} ${plan.version}?';
    return AlertDialog(
      title: Text(title),
      content: SizedBox(
        width: 560,
        child: _done
            ? _Installed(plan: plan!)
            : plan == null
            ? _AskSource(
                source: _source,
                sha: _sha,
                onSubmit: _busy ? null : _review,
              )
            : BundlePlanView(preview: p!),
      ),
      actions: [
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: Text(
              _error!,
              style: TextStyle(color: context.colors.error, fontSize: 12.5),
            ),
          ),
        if (newKey && !_done)
          Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Checkbox(
                value: _acceptKey,
                onChanged: (v) => setState(() => _acceptKey = v ?? false),
              ),
              const Text('I know why the key changed'),
            ],
          ),
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(_done ? 'Done' : 'Cancel'),
        ),
        if (!_done && plan == null)
          FilledButton(
            onPressed: _busy ? null : _review,
            child: _busy
                ? const SizedBox(
                    width: 14,
                    height: 14,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('Review'),
          ),
        if (!_done && plan != null && !upToDate)
          FilledButton(
            onPressed: _busy || !p!.canInstall || (newKey && !_acceptKey)
                ? null
                : _install,
            child: Text(plan.isUpdate ? 'Update' : 'Install'),
          ),
      ],
    );
  }
}

class _AskSource extends StatelessWidget {
  const _AskSource({
    required this.source,
    required this.sha,
    required this.onSubmit,
  });

  final TextEditingController source;
  final TextEditingController sha;
  final VoidCallback? onSubmit;

  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text(
        'Paste the link (https) or the path of a .tar.zst or folder that '
        'someone shared. Nothing is installed until you have seen what it '
        'can do.',
        style: TextStyle(color: context.colors.muted, height: 1.4),
      ),
      const SizedBox(height: 12),
      TextField(
        controller: source,
        autofocus: true,
        onSubmitted: (_) => onSubmit?.call(),
        decoration: const InputDecoration(
          labelText: 'Link or path',
          hintText: 'https://example.com/notion-recipe.tar.zst',
          isDense: true,
        ),
      ),
      const SizedBox(height: 8),
      TextField(
        controller: sha,
        onSubmitted: (_) => onSubmit?.call(),
        decoration: const InputDecoration(
          labelText: 'SHA-256 (optional: what the author published)',
          isDense: true,
        ),
      ),
    ],
  );
}

class _Installed extends StatelessWidget {
  const _Installed({required this.plan});

  final BundlePlan plan;

  @override
  Widget build(BuildContext context) {
    final lines = [
      if (plan.connectors.isNotEmpty)
        'Connectors are saved inactive. Ask tern to activate one when you '
            'want it; you approve each.',
      if (plan.routines.isNotEmpty)
        'Routines are paused. Turn them on in Routines.',
      for (final s in plan.secrets.where((s) => !s.set))
        'Set the secret ${s.name} (Settings → Secrets) for ${s.usedBy.join(', ')}.',
      for (final c in plan.connections.where((c) => !c.connected))
        'Add a ${c.provider} account for ${c.connector} in Connections.',
    ];
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (final l in lines.isEmpty ? ['Installed.'] : lines)
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: Text('• $l', style: const TextStyle(height: 1.35)),
          ),
      ],
    );
  }
}

/// Everything a bundle can do, as the user approves it: signature, each
/// connector's hosts, credentials and tool effects, skills, routines, what
/// it needs, what changed since the installed version, and warnings.
class BundlePlanView extends StatelessWidget {
  const BundlePlanView({super.key, required this.preview});

  final BundlePreview preview;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final plan = preview.plan;
    final small = TextStyle(fontSize: 12.5, color: c.muted);
    Widget heading(String t) => Padding(
      padding: const EdgeInsets.only(top: 14, bottom: 6),
      child: Text(t, style: Theme.of(context).textTheme.labelSmall),
    );
    Widget warning(String t, {bool severe = false}) => Padding(
      padding: const EdgeInsets.only(top: 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.warning_amber_rounded,
            size: 14,
            color: severe ? c.error : c.warn,
          ),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              t,
              style: TextStyle(
                fontSize: 12.5,
                color: severe ? c.error : null,
                fontWeight: severe ? FontWeight.w500 : null,
              ),
            ),
          ),
        ],
      ),
    );
    final signed = plan.signatureState != 'unsigned';
    return ConstrainedBox(
      constraints: const BoxConstraints(maxHeight: 520),
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (plan.description.isNotEmpty) Text(plan.description),
            if (plan.author.isNotEmpty) Text('by ${plan.author}', style: small),
            const SizedBox(height: 6),
            _SignatureLine(
              signed: signed,
              trusted:
                  plan.signatureState == 'trusted' ||
                  plan.signatureState == 'pinned',
              words: plan.signatureWords,
            ),
            SelectableText(
              '${plan.source}\nsha256 ${plan.sha256}',
              style: small.copyWith(fontSize: 11.5),
            ),
            for (final x in preview.conflicts) warning(x, severe: true),
            if (plan.isUpdate) ...[
              heading('CHANGES FROM ${plan.installedVersion}'),
              if (plan.added.isEmpty &&
                  plan.removed.isEmpty &&
                  plan.changed.isEmpty)
                Text('None in what it can do.', style: small),
              for (final a in plan.added)
                Text('+ $a', style: TextStyle(fontSize: 12.5, color: c.warn)),
              for (final r in plan.removed) Text('− $r', style: small),
              for (final x in plan.changed) Text('~ $x', style: small),
            ],
            if (plan.connectors.isNotEmpty) ...[
              heading('CONNECTORS · INSTALLED INACTIVE'),
              for (final k in plan.connectors) ...[
                Text(
                  '${k.name} — ${k.description}',
                  style: const TextStyle(fontWeight: FontWeight.w500),
                ),
                Text(
                  'Reaches ${k.hosts.isEmpty ? 'nothing' : k.hosts.join(', ')} · ${k.rate}',
                  style: small,
                ),
                for (final a in k.auth)
                  Text('Sends $a (the script never sees it)', style: small),
                if (k.oauth != null)
                  Text('Signs in with ${k.oauth}', style: small),
                Text(
                  'Tools: ${k.tools.map((t) => '${t.name} (${t.effect})').join(', ')}',
                  style: small,
                ),
                for (final w in k.warnings)
                  warning(w, severe: w.startsWith('lookalike')),
                const SizedBox(height: 8),
              ],
            ],
            if (plan.skills.isNotEmpty) ...[
              heading('SKILLS'),
              for (final s in plan.skills) ...[
                Text('${s.name} — ${s.description}'),
                if (s.runnable.isNotEmpty)
                  warning('Can be run: ${s.runnable.join(', ')}'),
              ],
            ],
            if (plan.routines.isNotEmpty) ...[
              heading('ROUTINES · INSTALLED PAUSED'),
              for (final r in plan.routines) ...[
                Text(
                  '${r.name} — ${r.trigger}',
                  style: const TextStyle(fontWeight: FontWeight.w500),
                ),
                Text(
                  'Initiative ${r.initiative} · autonomy ${r.autonomy}'
                  '${r.limits.isEmpty ? '' : ' · ${r.limits.join(' · ')}'}',
                  style: small,
                ),
                Text('“${r.prompt}”', style: small, maxLines: 3),
                for (final n in r.notes) Text(n, style: small),
                const SizedBox(height: 6),
              ],
            ],
            if (plan.secrets.isNotEmpty || plan.connections.isNotEmpty) ...[
              heading('NEEDS · NOTHING SECRET COMES WITH IT'),
              for (final s in plan.secrets)
                Text(
                  'Secret ${s.name} for ${s.usedBy.join(', ')}'
                  '${s.set ? ' (set)' : ' (not set)'}',
                  style: small,
                ),
              for (final k in plan.connections)
                Text(
                  'A ${k.provider} account for ${k.connector}'
                  '${k.scopes.isEmpty ? '' : ' (${k.scopes.join(', ')})'}'
                  '${k.connected ? ' (connected)' : ''}',
                  style: small,
                ),
            ],
            if (plan.warnings.isNotEmpty) ...[
              heading('WARNINGS'),
              for (final w in plan.warnings)
                warning(w, severe: w.contains('lookalike')),
            ],
          ],
        ),
      ),
    );
  }
}
