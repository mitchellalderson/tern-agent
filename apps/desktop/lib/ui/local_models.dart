// The local models dialog: what is installed, downloads with progress, and
// adding a model by name (docs/local-models.md). Downloads run in the daemon,
// so they go on when this closes. Copy is short; warnings are plain words.
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

Future<void> showLocalModelsDialog(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => LocalModelsDialog(store: store),
    );

/// "16 GB", "4.7 GB", "820 MB".
String sizeText(int bytes) {
  const gb = 1 << 30;
  if (bytes >= gb) {
    final v = bytes / gb;
    return '${v >= 10 ? v.toStringAsFixed(0) : v.toStringAsFixed(1)} GB';
  }
  return '${(bytes / (1 << 20)).round()} MB';
}

/// The note under an installed model, or null when there is nothing to say.
/// A model that can't use tools can chat but can't run tasks; one that is
/// big for this Mac may be slow or crash.
String? modelNote(LocalModel m, {int? ramBytes}) {
  if (m.tools == false) {
    return "Can't use tools, so it can't run tasks. It can still chat.";
  }
  if (ramBytes != null && m.size > ramBytes * 0.7) {
    return 'May be slow or crash. This Mac has ${sizeText(ramBytes)}.';
  }
  return null;
}

class LocalModelsDialog extends StatefulWidget {
  const LocalModelsDialog({super.key, required this.store});

  final TernStore store;

  @override
  State<LocalModelsDialog> createState() => _LocalModelsDialogState();
}

class _LocalModelsDialogState extends State<LocalModelsDialog> {
  LocalStatus? _status;
  List<LocalModel> _models = const [];
  List<LocalPull> _pulls = const [];
  LocalCatalog _catalog = const LocalCatalog();
  final _name = TextEditingController();
  String? _error;
  var _busy = false;
  Timer? _poll;

  TernClient get _client => widget.store.client;

  @override
  void initState() {
    super.initState();
    _load();
    _poll = Timer.periodic(const Duration(milliseconds: 1500), (_) => _load());
  }

  @override
  void dispose() {
    _poll?.cancel();
    _name.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final status = await _client.localStatus();
      final pulls = await _client.localPulls();
      final models = status.running
          ? await _client.localModels()
          : const <LocalModel>[];
      final catalog = status.running
          ? await _client.localCatalog().catchError((Object _) => _catalog)
          : _catalog;
      if (!mounted) return;
      setState(() {
        _catalog = catalog;
        _status = status;
        _pulls = pulls;
        _models = models;
      });
    } on Object {
      // Keep what is shown; the next poll tries again.
    }
  }

  Future<void> _run(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
      await _load();
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _download() async {
    final name = _name.text.trim();
    if (name.isEmpty) return;
    await _run(() async {
      await _client.localPull(name);
      _name.clear();
    });
  }

  Future<void> _remove(LocalModel m) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${m.name}?'),
        content: Text(
          'This frees ${sizeText(m.size)}. You can download it again later.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    await _run(() => _client.localDelete(m.name));
  }

  Future<void> _useFor(LocalTier tier, String model) => _run(() async {
    await _client.localUseTier(tier, model);
    await widget.store.refreshInfo();
  });

  Future<void> _get(String model) => _run(() async {
    await _client.localPull(model);
  });

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = _status;
    final header = [
      if (s?.ramBytes != null) '${sizeText(s!.ramBytes!)} Mac',
      if (s?.diskFree != null) '${sizeText(s!.diskFree!)} free',
    ].join(' · ');
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560, maxHeight: 640),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              const Text(
                'Local models',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 4),
              Text(
                header.isEmpty
                    ? 'Private and offline, good for everyday tasks.'
                    : '$header. Private and offline, good for everyday tasks.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 14),
              Flexible(
                child: SingleChildScrollView(
                  child: s == null
                      ? const Padding(
                          padding: EdgeInsets.all(24),
                          child: Center(
                            child: CircularProgressIndicator(strokeWidth: 2),
                          ),
                        )
                      : _body(context, s),
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 8),
                Text(
                  e,
                  key: const ValueKey('local-error'),
                  style: TextStyle(fontSize: 12.5, color: c.error),
                ),
              ],
              const SizedBox(height: 12),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => Navigator.pop(context),
                  child: const Text('Done'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _body(BuildContext context, LocalStatus s) {
    final c = context.colors;
    if (!s.running) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            s.installed
                ? 'Ollama isn\'t running.'
                : 'Ollama isn\'t installed yet.',
          ),
          const SizedBox(height: 10),
          if (s.installed)
            FilledButton(
              key: const ValueKey('local-start'),
              onPressed: _busy
                  ? null
                  : () => _run(() async {
                      await _client.localStart();
                    }),
              child: const Text('Start Ollama'),
            )
          else
            TextButton(
              onPressed: () => openUrl(s.installPage),
              child: const Text('Get it from ollama.com ↗'),
            ),
        ],
      );
    }
    final active = [
      for (final p in _pulls)
        if (!p.finished || p.phase == 'failed') p,
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final w in s.warnings)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: Text(w, style: TextStyle(fontSize: 12.5, color: c.warn)),
          ),
        if (_catalog.tiers.isNotEmpty) ...[
          _RecommendedSection(
            catalog: _catalog,
            busy: _busy,
            downloading: _pulls.where((p) => !p.finished).map((p) => p.model),
            onGet: _get,
            onUse: _useFor,
            onSetUpAll: () => _run(() async {
              await _client.localSetUpAll();
            }),
          ),
          const SizedBox(height: 12),
        ],
        Row(
          children: [
            Expanded(
              child: TextField(
                key: const ValueKey('local-name'),
                controller: _name,
                autocorrect: false,
                enableSuggestions: false,
                style: mono.copyWith(fontSize: 13),
                onSubmitted: (_) => _busy ? null : _download(),
                decoration: const InputDecoration(
                  isDense: true,
                  labelText: 'Add a model',
                  hintText: 'for example qwen3:8b',
                  border: OutlineInputBorder(),
                ),
              ),
            ),
            const SizedBox(width: 8),
            FilledButton(
              key: const ValueKey('local-download'),
              onPressed: _busy ? null : _download,
              child: const Text('Download'),
            ),
          ],
        ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            onPressed: () => openUrl('https://ollama.com/library'),
            child: const Text('Browse ollama.com/library ↗'),
          ),
        ),
        for (final p in active) _PullRow(pull: p, onCancel: () => _cancel(p)),
        if (_models.isEmpty && active.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Text(
              'No models yet. Add one above.',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
        for (final m in _models)
          _ModelRow(
            model: m,
            note: modelNote(m, ramBytes: s.ramBytes),
            onUse: (tier) => _useFor(tier, m.name),
            onRemove: () => _remove(m),
          ),
      ],
    );
  }

  Future<void> _cancel(LocalPull p) =>
      p.finished ? Future.value() : _run(() => _client.localCancelPull(p.id));
}

class _PullRow extends StatelessWidget {
  const _PullRow({required this.pull, required this.onCancel});

  final LocalPull pull;
  final VoidCallback onCancel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final failed = pull.phase == 'failed';
    final f = pull.fraction;
    final what = switch (pull.phase) {
      'verifying' => 'Checking…',
      'starting' => 'Starting…',
      _ =>
        f == null
            ? 'Downloading…'
            : '${(f * 100).round()}% · ${sizeText(pull.completed)} of '
                  '${sizeText(pull.total)}',
    };
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  pull.model,
                  style: const TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              if (!failed)
                TextButton(
                  key: ValueKey('cancel-${pull.id}'),
                  onPressed: onCancel,
                  child: const Text('Cancel'),
                ),
            ],
          ),
          if (failed)
            Text(
              pull.error ?? 'The download failed.',
              style: TextStyle(fontSize: 12.5, color: c.error),
            )
          else ...[
            LinearProgressIndicator(
              value: pull.phase == 'verifying' ? null : f,
              minHeight: 3,
            ),
            const SizedBox(height: 3),
            Text(what, style: TextStyle(fontSize: 11.5, color: c.muted)),
          ],
        ],
      ),
    );
  }
}

class _ModelRow extends StatelessWidget {
  const _ModelRow({
    required this.model,
    required this.note,
    required this.onUse,
    required this.onRemove,
  });

  final LocalModel model;
  final String? note;
  final void Function(LocalTier tier) onUse;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final detail = [
      sizeText(model.size),
      ?model.parameterSize,
      ?model.quantization,
      if (model.loaded) 'in memory',
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  model.name,
                  style: const TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                Text(detail, style: TextStyle(fontSize: 11.5, color: c.muted)),
                if (note case final n?)
                  Padding(
                    padding: const EdgeInsets.only(top: 2),
                    child: Text(
                      n,
                      style: TextStyle(fontSize: 11.5, color: c.warn),
                    ),
                  ),
              ],
            ),
          ),
          PopupMenuButton<VoidCallback>(
            tooltip: 'Actions',
            icon: Icon(Icons.more_horiz, size: 18, color: c.muted),
            onSelected: (f) => f(),
            itemBuilder: (_) => [
              for (final tier in LocalTier.values)
                PopupMenuItem(
                  value: () => onUse(tier),
                  enabled: !(tier.needsTools && model.tools == false),
                  child: Text('Use for ${tier.label}'),
                ),
              PopupMenuItem(value: onRemove, child: const Text('Remove…')),
            ],
          ),
        ],
      ),
    );
  }
}

/// "Recommended for this Mac": one model per tier, with what is in use.
class _RecommendedSection extends StatelessWidget {
  const _RecommendedSection({
    required this.catalog,
    required this.busy,
    required this.downloading,
    required this.onGet,
    required this.onUse,
    required this.onSetUpAll,
  });

  final LocalCatalog catalog;
  final bool busy;
  final Iterable<String> downloading;
  final void Function(String model) onGet;
  final void Function(LocalTier tier, String model) onUse;
  final VoidCallback onSetUpAll;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final all = catalog.tiers.every((t) => t.inUse);
    final loading = downloading.toSet();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            const Expanded(
              child: Text(
                'Recommended for this Mac',
                style: TextStyle(fontSize: 13, fontWeight: FontWeight.w600),
              ),
            ),
            if (!all)
              TextButton(
                key: const ValueKey('local-setup-all'),
                onPressed: busy || catalog.settingUp ? null : onSetUpAll,
                child: Text(
                  catalog.settingUp ? 'Setting up…' : 'Set up all three',
                ),
              ),
          ],
        ),
        if (catalog.error case final e?)
          Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
        for (final t in catalog.tiers)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 5),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Tooltip(
                        message: t.tier.hint,
                        child: Text(
                          '${t.tier.label} · ${t.model} · ${t.sizeGb} GB',
                          style: const TextStyle(fontSize: 13),
                        ),
                      ),
                      Text(
                        t.blurb,
                        style: TextStyle(fontSize: 11.5, color: c.muted),
                      ),
                      if (t.note case final n?)
                        Text(
                          n,
                          style: TextStyle(fontSize: 11.5, color: c.warn),
                        ),
                    ],
                  ),
                ),
                if (t.inUse)
                  Padding(
                    padding: const EdgeInsets.only(top: 6),
                    child: Text(
                      'In use',
                      style: TextStyle(fontSize: 12, color: c.ok),
                    ),
                  )
                else if (t.installed)
                  TextButton(
                    key: ValueKey('use-${t.tier.wire}'),
                    onPressed: busy ? null : () => onUse(t.tier, t.model),
                    child: Text('Use for ${t.tier.label}'),
                  )
                else if (loading.contains(t.model))
                  Padding(
                    padding: const EdgeInsets.only(top: 6),
                    child: Text(
                      'Downloading…',
                      style: TextStyle(fontSize: 12, color: c.muted),
                    ),
                  )
                else
                  TextButton(
                    key: ValueKey('get-${t.tier.wire}'),
                    onPressed: busy ? null : () => onGet(t.model),
                    child: const Text('Download'),
                  ),
              ],
            ),
          ),
      ],
    );
  }
}
