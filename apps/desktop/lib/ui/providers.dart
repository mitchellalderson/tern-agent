import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
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
  const ProvidersDialog({super.key, required this.store});

  final TernStore store;

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
      _notice = 'Saved ${p.name}${p.keyHint == null ? '' : ' (key ${p.keyHint})'}.';
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
      builder: (context) => _DefaultModelDialog(store: store, provider: provider),
    );
    if (chosen == null) return;
    try {
      final r = await _client.setDefaultModel(chosen);
      await store.refreshInfo();
      await _load();
      if (mounted) {
        setState(
          () => _notice = [
            'Default model: ${r.model}.',
            ?r.warning,
          ].join(' '),
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
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 600, maxHeight: 680),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 14),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
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
              Flexible(
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
                                ? const CircularProgressIndicator(
                                    strokeWidth: 2,
                                  )
                                : Text(
                                    _error!,
                                    style: TextStyle(color: c.error),
                                  ),
                          ),
                        )
                      : Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            _DefaultRow(
                              model: widget.store.info?.defaultModel,
                              onChange: _chooseDefault,
                            ),
                            const SizedBox(height: 10),
                            const Hairline(),
                            for (final p in providers)
                              _ProviderRow(
                                provider: p,
                                onChangeKey: () =>
                                    setState(() => _setup = (existing: p)),
                                onDefault: () =>
                                    _chooseDefault(provider: p.name),
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
              Row(
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
                        'Add provider',
                        style: TextStyle(color: c.muted),
                      ),
                    ),
                  const Spacer(),
                  TextButton(
                    onPressed: () {
                      // The navigator outlives this dialog's context.
                      final nav = Navigator.of(context);
                      nav.pop();
                      showSecretsDialog(nav.context, widget.store);
                    },
                    child: Text(
                      'Secrets (advanced)…',
                      style: TextStyle(color: c.muted),
                    ),
                  ),
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Done'),
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

class _DefaultRow extends StatelessWidget {
  const _DefaultRow({required this.model, required this.onChange});

  final String? model;
  final VoidCallback onChange;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Row(
      children: [
        Text('Default model', style: TextStyle(fontSize: 13, color: c.muted)),
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
        TextButton(onPressed: onChange, child: const Text('Change')),
      ],
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
      p.kind.label,
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
                      p.name,
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

/// Add a provider (kind → server → key → model), or change an existing
/// one's key. The key is checked with the provider before anything is
/// saved. Used in the Models dialog and the first-run welcome.
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
  late var _kind = widget.existing?.kind ?? ProviderKind.anthropic;
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

  @override
  void dispose() {
    _debounce?.cancel();
    _key.clear();
    _url.dispose();
    _name.dispose();
    _key.dispose();
    super.dispose();
  }

  ProviderDraft get _draft => ProviderDraft(
    kind: widget.existing == null ? _kind : null,
    name: _verified?.name ?? _name.text,
    baseUrl: _verified?.suggestedBaseUrl ?? _url.text,
    key: _useEnv ? null : _key.text,
    useEnvKey: _useEnv,
  );

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
    final needsKey =
        _kind == ProviderKind.anthropic || widget.existing != null;
    if (!_useEnv && needsKey && _key.text.trim().isEmpty) {
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
          _error = [
            v.message ?? "That key didn't work.",
            ...v.warnings,
          ].join(' ');
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
        _makeDefault = widget.store.info?.defaultModel == null ||
            widget.store.setupNeeded;
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
      if (mounted) widget.onDone(status);
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
      _step = _kind == ProviderKind.openaiCompat ? _Step.endpoint : _Step.kind;
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
          child: Text(widget.existing == null ? 'Check key' : 'Save key'),
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

  Widget _kindStep(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('Connect a model provider'),
      for (final (kind, blurb) in [
        (ProviderKind.anthropic, 'Claude models'),
        (
          ProviderKind.openaiCompat,
          'OpenAI, OpenRouter, Ollama, or any server with the OpenAI API',
        ),
      ])
        _KindCard(
          kind: kind,
          blurb: blurb,
          onTap: () => setState(() {
            _kind = kind;
            _error = null;
            _step = kind == ProviderKind.anthropic
                ? _Step.key
                : _Step.endpoint;
          }),
        ),
    ],
  );

  Widget _endpointStep(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title(
        'OpenAI-compatible server',
        'For example https://api.openai.com/v1, https://openrouter.ai/api/v1, '
            'or http://localhost:11434/v1 for Ollama.',
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
          hint: 'from the URL, e.g. openrouter',
        ),
      ),
    ],
  );

  Widget _keyStep(BuildContext context) {
    final c = context.colors;
    final existing = widget.existing;
    final (title, sub) = switch ((_kind, existing)) {
      (_, final p?) => ('New API key for ${p.name}', null),
      (ProviderKind.anthropic, null) => ('Paste your Anthropic API key', null),
      (ProviderKind.openaiCompat, null) => (
        'API key',
        "For ${_url.text.trim()}. Leave it blank if the server doesn't need one.",
      ),
    };
    final v = _verified;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title(title, sub),
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
            hint: _kind == ProviderKind.anthropic ? 'sk-ant-…' : 'Paste the key',
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
        const SizedBox(height: 8),
        Wrap(
          crossAxisAlignment: WrapCrossAlignment.center,
          spacing: 4,
          children: [
            if (_kind == ProviderKind.anthropic && existing == null) ...[
              Text(
                "Don't have one?",
                style: TextStyle(fontSize: 12.5, color: c.muted),
              ),
              TextButton(
                onPressed: () => openUrl(anthropicKeyUrl),
                child: const Text('Get an API key ↗'),
              ),
            ],
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
              ? '${v.name} answered — ${v.models.length} models'
              : 'Key ${v.keyHint} works — ${v.models.length} models',
        ),
        for (final n in notes)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(n, style: TextStyle(fontSize: 12.5, color: c.warn)),
          ),
        const SizedBox(height: 12),
        _title('Choose a model', 'Click one, or type a model id and press ⏎.'),
        ModelList(
          lists: [
            ProviderModels(
              provider: v.name,
              error: v.error,
              message: v.message,
              models: v.models,
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

class _KindCard extends StatelessWidget {
  const _KindCard({
    required this.kind,
    required this.blurb,
    required this.onTap,
  });

  final ProviderKind kind;
  final String blurb;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(10),
        child: Container(
          padding: const EdgeInsets.all(14),
          decoration: BoxDecoration(
            border: Border.all(color: c.hairline),
            borderRadius: BorderRadius.circular(10),
          ),
          child: Row(
            children: [
              Icon(
                kind == ProviderKind.anthropic
                    ? Icons.auto_awesome_outlined
                    : Icons.dns_outlined,
                size: 20,
                color: c.accent,
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      kind.label,
                      style: const TextStyle(
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      blurb,
                      style: TextStyle(fontSize: 12, color: c.muted),
                    ),
                  ],
                ),
              ),
              Icon(Icons.chevron_right, size: 18, color: c.muted),
            ],
          ),
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
    focusedBorder: border(c.accent),
  );
}
