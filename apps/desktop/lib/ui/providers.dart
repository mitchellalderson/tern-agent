import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'model_chain_editor.dart';
import 'model_picker.dart';
import 'secrets.dart';
import 'widgets.dart';

const anthropicKeyUrl = 'https://console.anthropic.com/settings/keys';

/// ⌘, : model providers, their keys and the default model
/// (docs/provider-setup-spec.md §7.2).
Future<void> showProvidersDialog(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => ProvidersDialog(store: store),
    );

class ProvidersDialog extends StatefulWidget {
  const ProvidersDialog({
    super.key,
    required this.store,
    this.embedded = false,
    this.onOpenSecrets,
  });

  final TernStore store;

  /// Shown inside Settings: no window chrome, title or Done; Settings
  /// supplies those, and [onOpenSecrets] switches to its Secrets section.
  final bool embedded;
  final VoidCallback? onOpenSecrets;

  @override
  State<ProvidersDialog> createState() => _ProvidersDialogState();
}

class _ProvidersDialogState extends State<ProvidersDialog> {
  List<ProviderStatus>? _providers;
  String? _error;
  String? _notice;

  /// Adding (null provider) or changing the key of a provider.
  ({ProviderStatus? existing})? _setup;

  TernClient get _client => widget.store.client;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    if (widget.store.daemonTooOld) {
      // Called from initState too, before the first build.
      _error = TernStore.daemonTooOldMessage;
      return;
    }
    try {
      final list = await _client.providers();
      if (!mounted) return;
      setState(() {
        _providers = list;
        // Nothing to manage yet: go straight to adding one.
        if (list.isEmpty) _setup = (existing: null);
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _saved(ProviderStatus p) async {
    await widget.store.refreshInfo();
    if (widget.store.modelLists != null) unawaited(widget.store.loadModels());
    await _load();
    if (!mounted) return;
    setState(() {
      _setup = null;
      _notice =
          'Saved ${p.name}${p.keyHint == null ? '' : ' (key ${p.keyHint})'}.';
    });
  }

  Future<void> _remove(ProviderStatus p) async {
    var removeKey = true;
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setLocal) => AlertDialog(
          title: Text('Remove ${p.name}?'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                p.isDefault
                    ? "It's the default provider, so new conversations "
                          'will need another model.'
                    : 'Conversations using it will need another model.',
              ),
              if (p.secret != null && p.keySource == 'file')
                CheckboxListTile(
                  value: removeKey,
                  contentPadding: EdgeInsets.zero,
                  controlAffinity: ListTileControlAffinity.leading,
                  title: Text('Also delete its API key (${p.secret})'),
                  onChanged: (v) => setLocal(() => removeKey = v!),
                ),
            ],
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
      ),
    );
    if (!(ok ?? false)) return;
    try {
      await _client.removeProvider(p.name, removeSecret: removeKey);
      await widget.store.refreshInfo();
      await _load();
      if (mounted) setState(() => _notice = 'Removed ${p.name}.');
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _chooseDefault({String? provider}) async {
    final store = widget.store;
    unawaited(store.loadModels());
    final chosen = await showDialog<String>(
      context: context,
      builder: (context) =>
          _DefaultModelDialog(store: store, provider: provider),
    );
    if (chosen == null) return;
    try {
      final r = await _client.setDefaultModel(chosen);
      await store.refreshInfo();
      await _load();
      if (mounted) {
        setState(
          () => _notice = ['Default model: ${r.model}.', ?r.warning].join(' '),
        );
      }
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _refresh() async {
    await widget.store.loadModels(refresh: true);
    await _load();
    if (mounted) setState(() => _notice = 'Model lists refreshed.');
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final providers = _providers;
    final setup = _setup;
    final embedded = widget.embedded;
    final content = Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (!embedded) ...[
          const Text(
            'Models',
            style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
          ),
          const SizedBox(height: 4),
          Text(
            'The providers tern can use, their API keys, and the model '
            'new conversations start with. Changes apply to the next '
            'message.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
          const SizedBox(height: 14),
        ],
        MaybeFlexible(
          embedded: embedded,
          child: SingleChildScrollView(
            child: setup != null
                ? ProviderSetup(
                    store: widget.store,
                    existing: setup.existing,
                    onDone: _saved,
                    onCancel: (providers?.isEmpty ?? true)
                        ? null
                        : () => setState(() => _setup = null),
                  )
                : providers == null
                ? Padding(
                    padding: const EdgeInsets.all(24),
                    child: Center(
                      child: _error == null
                          ? const CircularProgressIndicator(strokeWidth: 2)
                          : Text(_error!, style: TextStyle(color: c.error)),
                    ),
                  )
                : Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      _DefaultModelSection(
                        store: widget.store,
                        onChange: _chooseDefault,
                        onNotice: (n) => setState(() {
                          _error = null;
                          _notice = n;
                        }),
                      ),
                      const SizedBox(height: 10),
                      const Hairline(),
                      for (final p in providers)
                        _ProviderRow(
                          provider: p,
                          onChangeKey: () =>
                              setState(() => _setup = (existing: p)),
                          onDefault: () => _chooseDefault(provider: p.name),
                          onRefresh: _refresh,
                          onRemove: () => _remove(p),
                        ),
                    ],
                  ),
          ),
        ),
        for (final (text, color) in [
          if (_error case final e? when providers != null) (e, c.error),
          if (_notice case final n? when setup == null) (n, c.ok),
        ]) ...[
          const SizedBox(height: 10),
          Text(text, style: TextStyle(fontSize: 12.5, color: color)),
        ],
        const SizedBox(height: 12),
        Wrap(
          alignment: setup == null
              ? WrapAlignment.spaceBetween
              : WrapAlignment.end,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            if (setup == null)
              TextButton.icon(
                onPressed: providers == null
                    ? null
                    : () => setState(() {
                        _notice = null;
                        _setup = (existing: null);
                      }),
                icon: Icon(Icons.add, size: 16, color: c.muted),
                label: Text(
                  'Connect a provider',
                  style: TextStyle(color: c.muted),
                ),
              ),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (embedded && widget.onOpenSecrets == null)
                  const SizedBox.shrink()
                else
                  TextButton(
                    onPressed: embedded
                        ? widget.onOpenSecrets
                        : () {
                            // The navigator outlives this dialog's
                            // context.
                            final nav = Navigator.of(context);
                            nav.pop();
                            showSecretsDialog(nav.context, widget.store);
                          },
                    child: Text(
                      embedded ? 'Secrets…' : 'Secrets (advanced)…',
                      style: TextStyle(color: c.muted),
                    ),
                  ),
                if (!embedded)
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Done'),
                  ),
              ],
            ),
          ],
        ),
      ],
    );
    if (embedded) return content;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 600, maxHeight: 680),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 14),
          child: content,
        ),
      ),
    );
  }
}

/// The default model and its fallbacks (T6a): the primary with Change (as
/// before), the fallbacks in a line, and the visual editor behind
/// "Edit fallbacks", which writes `default_model` as a list.
class _DefaultModelSection extends StatefulWidget {
  const _DefaultModelSection({
    required this.store,
    required this.onChange,
    required this.onNotice,
  });

  final TernStore store;
  final VoidCallback onChange;
  final ValueChanged<String> onNotice;

  @override
  State<_DefaultModelSection> createState() => _DefaultModelSectionState();
}

class _DefaultModelSectionState extends State<_DefaultModelSection> {
  /// The saved chain, once read.
  List<String>? _saved;

  /// What the editor shows while editing; null when not editing.
  List<String>? _draft;
  String? _error;
  var _busy = false;

  /// The default model the chain was read for.
  String? _loadedFor;

  TernStore get _store => widget.store;

  /// Fallbacks may be edited by an owner (a config write); a paired chat
  /// device is told so instead of offered a button that fails.
  bool get _canEdit => _store.health?.isOwner ?? true;

  @override
  void initState() {
    super.initState();
    _loadedFor = _store.info?.defaultModel;
    _load();
    // Loading notifies, so not during this build.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && _store.modelLists == null && !_store.loadingModels) {
        unawaited(_store.loadModels());
      }
    });
  }

  Future<void> _load() async {
    final chain = await _store.loadDefaultChain();
    if (mounted && _draft == null) setState(() => _saved = chain);
  }

  bool get _changed =>
      _draft != null && _draft!.join('\n') != (_saved ?? const []).join('\n');

  Future<void> _save() async {
    final draft = _draft;
    if (draft == null) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await _store.saveDefaultChain(draft);
    if (!mounted) return;
    if (err != null) {
      setState(() {
        _busy = false;
        _error = err;
      });
      return;
    }
    setState(() {
      _busy = false;
      _saved = draft;
      _draft = null;
    });
    widget.onNotice(
      draft.length > 1
          ? 'Default model: ${draft.first}, then ${draft.skip(1).join(', ')}.'
          : 'Default model: ${draft.first}, with no fallbacks.',
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return ListenableBuilder(
      listenable: _store,
      builder: (context, _) {
        final model = _store.info?.defaultModel;
        // Another primary was chosen (Change, or a provider's default):
        // read the chain again, once.
        if (_loadedFor != model) {
          _loadedFor = model;
          WidgetsBinding.instance.addPostFrameCallback((_) => _load());
        }
        final chain = _saved ?? [?model];
        final draft = _draft;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Text(
                  'Default model',
                  style: TextStyle(fontSize: 13, color: c.muted),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Text(
                    model ?? 'None yet',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: model == null
                        ? TextStyle(fontSize: 13, color: c.warn)
                        : mono.copyWith(fontSize: 13),
                  ),
                ),
                TextButton(
                  onPressed: draft == null ? widget.onChange : null,
                  child: const Text('Change'),
                ),
              ],
            ),
            if (draft == null && model != null)
              Row(
                children: [
                  Expanded(
                    child: Text(
                      chain.length > 1
                          ? 'Then, if it fails: ${chain.skip(1).join(' → ')}'
                          : 'No fallbacks: if it fails, the message fails.',
                      key: const Key('default-fallbacks-summary'),
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
                  if (_canEdit)
                    TextButton(
                      key: const Key('edit-default-fallbacks'),
                      onPressed: () => setState(() {
                        _draft = [...chain];
                        _error = null;
                      }),
                      child: Text(
                        chain.length > 1 ? 'Edit fallbacks' : 'Add fallbacks',
                      ),
                    ),
                ],
              ),
            if (draft != null) ...[
              const SizedBox(height: 6),
              ModelChainEditor(
                store: _store,
                chain: draft,
                enabled: !_busy,
                onChanged: (v) => setState(() {
                  _draft = v;
                  _error = null;
                }),
              ),
              if (_error case final e?)
                Padding(
                  padding: const EdgeInsets.only(bottom: 6),
                  child: Text(
                    e,
                    key: const Key('default-chain-error'),
                    style: TextStyle(fontSize: 12.5, color: c.error),
                  ),
                ),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  TextButton(
                    key: const Key('cancel-default-fallbacks'),
                    onPressed: _busy
                        ? null
                        : () => setState(() {
                            _draft = null;
                            _error = null;
                          }),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 6),
                  FilledButton(
                    key: const Key('save-default-fallbacks'),
                    onPressed:
                        _busy ||
                            !_changed ||
                            chainBlocked(
                              _store,
                              draft,
                              saved: _saved ?? const [],
                            )
                        ? null
                        : _save,
                    child: Text(_busy ? 'Saving…' : 'Save'),
                  ),
                ],
              ),
            ],
          ],
        );
      },
    );
  }
}

class _ProviderRow extends StatelessWidget {
  const _ProviderRow({
    required this.provider,
    required this.onChangeKey,
    required this.onDefault,
    required this.onRefresh,
    required this.onRemove,
  });

  final ProviderStatus provider;
  final VoidCallback onChangeKey;
  final VoidCallback onDefault;
  final VoidCallback onRefresh;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final p = provider;
    final (status, color) = switch ((p.keySource, p.error?.kind)) {
      ('missing', _) => ('Key missing', c.error),
      (_, 'unauthorized') => ('Key rejected', c.error),
      (_, 'unreachable') => ('Unreachable', c.error),
      ('env', _) => ('Key from environment', c.ok),
      ('none', _) => ('No key', c.ok),
      _ => ('Ready', c.ok),
    };
    final detail = [
      if (p.label != p.name) p.name,
      if (p.preset == null || p.preset == 'other') p.kind.label,
      if (p.kind == ProviderKind.openaiCompat) ?p.baseUrl,
      if (p.keyHint case final h?) 'key $h',
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 2),
            child: Icon(Icons.hub_outlined, size: 17, color: c.accent),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Text(
                      p.label,
                      style: const TextStyle(
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    if (p.isDefault) ...[
                      const SizedBox(width: 6),
                      _Chip('Default', c.accent),
                    ],
                    const SizedBox(width: 6),
                    _Chip(status, color),
                  ],
                ),
                const SizedBox(height: 2),
                Text(
                  detail,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 11.5, color: c.muted),
                ),
                if (p.message case final m?) ...[
                  const SizedBox(height: 2),
                  Text(m, style: TextStyle(fontSize: 11.5, color: c.warn)),
                ],
              ],
            ),
          ),
          PopupMenuButton<VoidCallback>(
            tooltip: 'Actions',
            icon: Icon(Icons.more_horiz, size: 18, color: c.muted),
            onSelected: (f) => f(),
            itemBuilder: (_) => [
              PopupMenuItem(
                value: onChangeKey,
                child: Text(
                  p.kind == ProviderKind.openaiCompat && p.secret == null
                      ? 'Add an API key…'
                      : 'Change API key…',
                ),
              ),
              PopupMenuItem(
                value: onDefault,
                child: const Text('Choose default model…'),
              ),
              PopupMenuItem(
                value: onRefresh,
                child: const Text('Refresh models'),
              ),
              PopupMenuItem(value: onRemove, child: const Text('Remove…')),
            ],
          ),
        ],
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip(this.label, this.color);

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
    decoration: BoxDecoration(
      color: color.withValues(alpha: 0.12),
      borderRadius: BorderRadius.circular(6),
    ),
    child: Text(label, style: TextStyle(fontSize: 11, color: color)),
  );
}

/// Pick a model to make the default, from one provider or all of them.
class _DefaultModelDialog extends StatelessWidget {
  const _DefaultModelDialog({required this.store, this.provider});

  final TernStore store;
  final String? provider;

  @override
  Widget build(BuildContext context) => Dialog(
    shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 500),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(18, 16, 18, 16),
        child: ListenableBuilder(
          listenable: store,
          builder: (context, _) => Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                provider == null
                    ? 'Default model'
                    : 'Default model from $provider',
                style: const TextStyle(
                  fontSize: 16,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 10),
              ModelList(
                lists: store.modelLists
                    ?.where((l) => provider == null || l.provider == provider)
                    .toList(),
                loading: store.loadingModels,
                current: store.info?.defaultModel,
                onChoose: (m) => Navigator.pop(
                  context,
                  provider != null && !m.startsWith('$provider:')
                      ? '$provider:$m'
                      : m,
                ),
              ),
            ],
          ),
        ),
      ),
    ),
  );
}

enum _Step { kind, endpoint, key, model }

/// What the list shows when an older daemon can't send its presets: enough
/// to get connected. The daemon's own list (`/v1/providers/presets`) wins.
const fallbackPresets = [
  ProviderPreset(
    id: 'anthropic',
    label: 'Anthropic',
    blurb: 'Claude models',
    kind: ProviderKind.anthropic,
    name: 'anthropic',
    keyUrl: anthropicKeyUrl,
    keyHint: 'Create a key in the Anthropic Console, then paste it here.',
    needsKey: true,
    local: false,
    suggestedModels: [],
  ),
  ProviderPreset(
    id: 'other',
    label: 'Other (OpenAI-compatible URL)',
    blurb: 'Any server that speaks the OpenAI API',
    kind: ProviderKind.openaiCompat,
    name: 'custom',
    keyHint:
        "Enter the server's base URL. Leave the key blank if it doesn't "
        'need one.',
    needsKey: false,
    local: false,
    suggestedModels: [],
  ),
];

/// Connect a provider (choice → key → model), or change an existing one's
/// key. Starts from services people recognise (Anthropic, OpenAI,
/// OpenRouter, ...): picking one fills in the address and says where the
/// key comes from, so all that is left to ask for is the key. "Test
/// connection" calls the provider before anything is saved. Used in the
/// Models dialog and the first-run welcome.
class ProviderSetup extends StatefulWidget {
  const ProviderSetup({
    super.key,
    required this.store,
    this.existing,
    required this.onDone,
    this.onCancel,
  });

  final TernStore store;
  final ProviderStatus? existing;
  final ValueChanged<ProviderStatus> onDone;
  final VoidCallback? onCancel;

  @override
  State<ProviderSetup> createState() => ProviderSetupState();
}

class ProviderSetupState extends State<ProviderSetup> {
  late var _step = widget.existing == null ? _Step.kind : _Step.key;
  List<ProviderPreset>? _presets;
  ProviderPreset? _preset;
  final _url = TextEditingController();
  final _name = TextEditingController();
  final _key = TextEditingController();
  var _show = false;
  var _busy = false;
  var _useEnv = false;
  var _makeDefault = true;
  Verified? _verified;
  String? _error;
  Timer? _debounce;
  var _lastKeyLength = 0;

  TernClient get _client => widget.store.client;

  ProviderKind get _kind =>
      widget.existing?.kind ?? _preset?.kind ?? ProviderKind.anthropic;

  /// Whether the key may be left blank (a local server, or "Other").
  bool get _keyOptional =>
      widget.existing == null && !(_preset?.needsKey ?? true);

  /// What to call the provider in text.
  String get _label =>
      widget.existing?.label ??
      (_preset != null && !_preset!.isOther ? _preset!.label : null) ??
      _verified?.name ??
      'the server';

  @override
  void initState() {
    super.initState();
    if (widget.existing == null) _loadPresets();
  }

  Future<void> _loadPresets() async {
    List<ProviderPreset> list;
    try {
      list = await _client.providerPresets();
      if (list.isEmpty) list = fallbackPresets;
    } on Object {
      list = fallbackPresets;
    }
    if (mounted) setState(() => _presets = list);
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _key.clear();
    _url.dispose();
    _name.dispose();
    _key.dispose();
    super.dispose();
  }

  ProviderDraft get _draft {
    String? opt(String s) => s.trim().isEmpty ? null : s.trim();
    final preset = widget.existing == null ? _preset : null;
    return ProviderDraft(
      kind: widget.existing == null ? _kind : null,
      preset: preset?.id,
      // A preset's name and address come from the daemon, which also picks
      // a free name when that service is connected twice.
      name:
          _verified?.name ?? (preset?.isOther ?? true ? opt(_name.text) : null),
      baseUrl:
          _verified?.suggestedBaseUrl ??
          (preset?.isOther ?? true ? opt(_url.text) : null),
      key: _useEnv ? null : opt(_key.text),
      useEnvKey: _useEnv,
    );
  }

  void _pick(ProviderPreset p) => setState(() {
    _preset = p;
    _error = null;
    _verified = null;
    _step = p.isOther ? _Step.endpoint : _Step.key;
  });

  void _onKeyChanged(String text) {
    // A jump of several characters at once is a paste: check it shortly.
    final pasted = text.length - _lastKeyLength >= 8;
    _lastKeyLength = text.length;
    setState(() {
      _error = null;
      _verified = null;
      _useEnv = false;
    });
    _debounce?.cancel();
    if (pasted) {
      _debounce = Timer(const Duration(milliseconds: 400), _verify);
    }
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim() ?? '';
    if (text.isEmpty) return;
    _key.text = text;
    _lastKeyLength = text.length;
    setState(() {
      _error = null;
      _useEnv = false;
    });
    await _verify();
  }

  Future<void> _verify({bool env = false}) async {
    _debounce?.cancel();
    if (env) _useEnv = true;
    if (!_useEnv && !_keyOptional && _key.text.trim().isEmpty) {
      setState(() => _error = 'Paste your API key first.');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final v = await _client.verifyProvider(
        _draft,
        existing: widget.existing?.name,
      );
      if (!mounted) return;
      if (!v.ok) {
        setState(() {
          _busy = false;
          _error = [v.message ?? "That didn't work.", ...v.warnings].join(' ');
        });
        return;
      }
      _verified = v;
      if (widget.existing != null) {
        // Changing a key: nothing else to choose.
        await _save(null);
        return;
      }
      setState(() {
        _busy = false;
        _step = _Step.model;
        _makeDefault =
            widget.store.info?.defaultModel == null || widget.store.setupNeeded;
      });
    } on TernException catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = e.message;
        });
      }
    }
  }

  Future<void> _save(String? model) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final status = await _client.saveProvider(
        _draft,
        existing: widget.existing?.name,
        defaultModel: _makeDefault ? model : null,
      );
      _key.clear();
      if (!mounted) return;
      setState(() => _busy = false);
      widget.onDone(status);
    } on TernException catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = e.message;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final body = switch (_step) {
      _Step.kind => _kindStep(context),
      _Step.endpoint => _endpointStep(context),
      _Step.key => _keyStep(context),
      _Step.model => _modelStep(context),
    };
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        body,
        if (_error case final e?) ...[
          const SizedBox(height: 10),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(Icons.error_outline, size: 15, color: c.error),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  e,
                  key: const ValueKey('setup-error'),
                  style: TextStyle(fontSize: 12.5, color: c.error),
                ),
              ),
            ],
          ),
        ],
        const SizedBox(height: 12),
        Row(
          children: [
            if (_back() case final back?)
              TextButton(
                onPressed: _busy ? null : back,
                child: const Text('Back'),
              )
            else if (widget.onCancel case final cancel?)
              TextButton(
                onPressed: _busy ? null : cancel,
                child: const Text('Cancel'),
              ),
            const Spacer(),
            if (_busy)
              const Padding(
                padding: EdgeInsets.only(right: 12),
                child: SizedBox.square(
                  dimension: 16,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
              ),
            ..._actions(context),
          ],
        ),
      ],
    );
  }

  VoidCallback? _back() => switch (_step) {
    _Step.kind => null,
    _Step.endpoint => () => setState(() => _step = _Step.kind),
    _Step.key when widget.existing != null => null,
    _Step.key => () => setState(() {
      _error = null;
      _step = _preset?.isOther ?? false ? _Step.endpoint : _Step.kind;
    }),
    _Step.model => () => setState(() {
      _verified = null;
      _step = _Step.key;
    }),
  };

  List<Widget> _actions(BuildContext context) {
    final c = context.colors;
    final style = FilledButton.styleFrom(
      backgroundColor: c.accent,
      foregroundColor: c.onAccent,
    );
    return switch (_step) {
      _Step.kind => const [],
      _Step.endpoint => [
        FilledButton(
          style: style,
          onPressed: _url.text.trim().isEmpty
              ? null
              : () => setState(() {
                  _error = null;
                  _step = _Step.key;
                }),
          child: const Text('Next'),
        ),
      ],
      _Step.key => [
        FilledButton(
          style: style,
          onPressed: _busy ? null : _verify,
          child: Text(widget.existing == null ? 'Test connection' : 'Save key'),
        ),
      ],
      _Step.model => [
        TextButton(
          onPressed: _busy ? null : () => _save(null),
          child: const Text('Skip'),
        ),
      ],
    };
  }

  Widget _title(String text, [String? sub]) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(
        text,
        style: const TextStyle(fontSize: 14.5, fontWeight: FontWeight.w600),
      ),
      if (sub != null) ...[
        const SizedBox(height: 3),
        Text(sub, style: Theme.of(context).textTheme.bodySmall),
      ],
      const SizedBox(height: 12),
    ],
  );

  Widget _kindStep(BuildContext context) {
    final presets = _presets;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title(
          'Connect a provider',
          "Pick where your models come from. You'll only need an API key next.",
        ),
        if (presets == null)
          const Padding(
            padding: EdgeInsets.all(20),
            child: Center(
              child: SizedBox.square(
                dimension: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
            ),
          )
        else
          LayoutBuilder(
            builder: (context, box) {
              const gap = 8.0;
              final two = box.maxWidth >= 440;
              final width = two ? (box.maxWidth - gap) / 2 : box.maxWidth;
              return Wrap(
                spacing: gap,
                runSpacing: gap,
                children: [
                  for (final p in presets)
                    SizedBox(
                      width: width,
                      child: _PresetCard(preset: p, onTap: () => _pick(p)),
                    ),
                ],
              );
            },
          ),
      ],
    );
  }

  Widget _endpointStep(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title(
        'Other (OpenAI-compatible URL)',
        "The address of the server's API. It usually ends in /v1, for "
            'example http://localhost:8000/v1.',
      ),
      TextField(
        key: const ValueKey('base-url'),
        controller: _url,
        autofocus: true,
        autocorrect: false,
        enableSuggestions: false,
        style: mono.copyWith(fontSize: 13),
        onChanged: (_) => setState(() => _error = null),
        onSubmitted: (_) {
          if (_url.text.trim().isNotEmpty) setState(() => _step = _Step.key);
        },
        decoration: _box(context, label: 'Base URL', hint: 'https://…/v1'),
      ),
      const SizedBox(height: 10),
      TextField(
        key: const ValueKey('provider-name'),
        controller: _name,
        autocorrect: false,
        enableSuggestions: false,
        style: mono.copyWith(fontSize: 13),
        decoration: _box(
          context,
          label: 'Name (optional)',
          hint: 'from the URL, e.g. mylab',
        ),
      ),
    ],
  );

  Widget _keyStep(BuildContext context) {
    final c = context.colors;
    final existing = widget.existing;
    final preset = _preset;
    final hideKey = existing == null && preset != null && preset.local;
    final (title, sub) = switch ((existing, preset)) {
      (final p?, _) => ('New API key for ${p.label}', null),
      (null, final p?) when p.local => ('${p.label} needs no key', p.keyHint),
      (null, final p?) when !p.isOther => (
        'Paste your ${p.label} API key',
        p.keyHint,
      ),
      _ => (
        'API key (optional)',
        "For ${_url.text.trim()}. Leave it blank if the server doesn't need "
            'one.',
      ),
    };
    final v = _verified;
    final keyUrl = existing == null ? preset?.keyUrl : null;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title(title, sub),
        if (!hideKey)
          TextField(
            key: const ValueKey('api-key'),
            controller: _key,
            autofocus: true,
            obscureText: !_show,
            autocorrect: false,
            enableSuggestions: false,
            enableIMEPersonalizedLearning: false,
            style: mono.copyWith(fontSize: 13),
            onChanged: _onKeyChanged,
            onSubmitted: (_) => _busy ? null : _verify(),
            decoration: _box(
              context,
              hint: _kind == ProviderKind.anthropic
                  ? 'sk-ant-…'
                  : 'Paste the key',
              suffix: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  IconButton(
                    tooltip: _show ? 'Hide' : 'Show',
                    icon: Icon(
                      _show ? Icons.visibility_off : Icons.visibility,
                      size: 17,
                      color: c.muted,
                    ),
                    onPressed: () => setState(() => _show = !_show),
                  ),
                  TextButton(
                    onPressed: _busy ? null : _paste,
                    child: const Text('Paste'),
                  ),
                ],
              ),
            ),
          ),
        if (existing == null && preset != null && !preset.isOther) ...[
          const SizedBox(height: 8),
          Text(
            'Talks to ${preset.baseUrl ?? 'api.anthropic.com'}',
            key: const ValueKey('preset-url'),
            style: mono.copyWith(fontSize: 11.5, color: c.muted),
          ),
        ],
        const SizedBox(height: 6),
        Wrap(
          crossAxisAlignment: WrapCrossAlignment.center,
          spacing: 4,
          children: [
            if (keyUrl != null) ...[
              if (!hideKey)
                Text(
                  "Don't have one?",
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              TextButton(
                onPressed: () => openUrl(keyUrl),
                child: Text(hideKey ? 'Get it ↗' : 'Get an API key ↗'),
              ),
            ],
            if (!hideKey)
              TextButton(
                onPressed: _busy ? null : () => _verify(env: true),
                child: const Text('Use the key from the environment'),
              ),
          ],
        ),
        if (v != null && v.ok)
          _Success('Key ${v.keyHint ?? ''} works — ${v.models.length} models'),
      ],
    );
  }

  Widget _modelStep(BuildContext context) {
    final c = context.colors;
    final v = _verified!;
    final notes = [
      ?v.message,
      ...v.warnings,
      if (v.suggestedBaseUrl case final u?) 'Using $u (added /v1).',
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _Success(
          v.keyHint == null
              ? 'Connected to $_label — ${v.models.length} models'
              : 'Connected to $_label (key ${v.keyHint}) — '
                    '${v.models.length} models',
        ),
        for (final n in notes)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(n, style: TextStyle(fontSize: 12.5, color: c.warn)),
          ),
        const SizedBox(height: 12),
        _title('Choose a model', 'Click one, or search for it.'),
        ModelList(
          lists: [
            ProviderModels(
              provider: v.name,
              label: _label,
              error: v.error,
              message: v.message,
              models: v.models,
              suggested: _preset?.suggestedModels ?? const [],
            ),
          ],
          maxHeight: 260,
          onChoose: (m) => _busy ? null : _save(m),
        ),
        const SizedBox(height: 4),
        Row(
          children: [
            Checkbox(
              value: _makeDefault,
              visualDensity: VisualDensity.compact,
              onChanged: (x) => setState(() => _makeDefault = x!),
            ),
            Text(
              'Make it the default for new conversations',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ],
        ),
      ],
    );
  }
}

class _Success extends StatelessWidget {
  const _Success(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Row(
        children: [
          Icon(Icons.check_circle, size: 16, color: c.ok),
          const SizedBox(width: 6),
          Expanded(
            child: Text(text, style: TextStyle(fontSize: 12.5, color: c.ok)),
          ),
        ],
      ),
    );
  }
}

IconData _presetIcon(ProviderPreset p) => switch (p.id) {
  'anthropic' => Icons.auto_awesome_outlined,
  'other' => Icons.dns_outlined,
  _ when p.local => Icons.computer_outlined,
  _ => Icons.cloud_outlined,
};

class _PresetCard extends StatelessWidget {
  const _PresetCard({required this.preset, required this.onTap});

  final ProviderPreset preset;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return InkWell(
      key: ValueKey('preset-${preset.id}'),
      onTap: onTap,
      borderRadius: BorderRadius.circular(10),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 11),
        decoration: BoxDecoration(
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(10),
        ),
        child: Row(
          children: [
            Icon(_presetIcon(preset), size: 19, color: c.accent),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    preset.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    preset.blurb,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

InputDecoration _box(
  BuildContext context, {
  String? label,
  String? hint,
  Widget? suffix,
}) {
  final c = context.colors;
  OutlineInputBorder border(Color color) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(8),
    borderSide: BorderSide(color: color),
  );
  return InputDecoration(
    labelText: label,
    hintText: hint,
    hintStyle: TextStyle(color: c.muted),
    suffixIcon: suffix,
    isDense: true,
    contentPadding: const EdgeInsets.all(12),
    border: border(c.hairline),
    enabledBorder: border(c.hairline),
    focusedBorder: border(c.ink),
  );
}
