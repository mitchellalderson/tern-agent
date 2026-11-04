import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'mcp.dart';
import 'widgets.dart';

/// Names the daemon accepts on write (`tern_core::mcp::check_name`).
final _serverName = RegExp(r'^[a-z0-9_-]{1,24}$');

const remoteStdioOff =
    'Adding local commands from another device is off. Turn on '
    '`api.remote_mcp_stdio` on the host.';

/// Add a server (blank, or one tab per pasted draft) or edit `editing`
/// (docs/mcp-management-spec.md §5).
Future<void> showMcpSheet(
  BuildContext context,
  TernStore store, {
  McpStatus? editing,
  List<ImportedServer> drafts = const [],
}) => showDialog<void>(
  context: context,
  builder: (_) => McpSheet(store: store, editing: editing, drafts: drafts),
);

/// Paste a Claude Desktop / Cursor style `mcpServers` snippet, then review
/// it in the add sheet.
Future<void> showMcpPaste(BuildContext context, TernStore store) async {
  final drafts = await showDialog<List<ImportedServer>>(
    context: context,
    builder: (_) => McpPasteDialog(store: store),
  );
  if (drafts == null || drafts.isEmpty || !context.mounted) return;
  await showMcpSheet(context, store, drafts: drafts);
}

class McpPasteDialog extends StatefulWidget {
  const McpPasteDialog({super.key, required this.store});

  final TernStore store;

  @override
  State<McpPasteDialog> createState() => _McpPasteDialogState();
}

class _McpPasteDialogState extends State<McpPasteDialog> {
  final _text = TextEditingController();
  String? _error;
  var _busy = false;

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    if (data?.text case final t? when t.trim().isNotEmpty) {
      _text.text = t;
      await _import();
    }
  }

  Future<void> _import() async {
    if (_text.text.trim().isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final drafts = await widget.store.client.importMcp(_text.text);
      if (mounted) Navigator.pop(context, drafts);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return AlertDialog(
      title: const Text('Paste an MCP config'),
      content: SizedBox(
        width: 560,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              'The JSON a server documents for Claude Desktop or Cursor '
              '(`"mcpServers": {…}`). Keys in it become secrets; nothing is '
              'saved until you review it.',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
            const SizedBox(height: 10),
            TextField(
              key: const ValueKey('mcp-paste'),
              controller: _text,
              autofocus: true,
              minLines: 8,
              maxLines: 14,
              autocorrect: false,
              enableSuggestions: false,
              enableIMEPersonalizedLearning: false,
              style: mono.copyWith(fontSize: 12.5),
              decoration: _box(context, hint: '{ "mcpServers": { … } }'),
            ),
            if (_error case final e?) ...[
              const SizedBox(height: 8),
              Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : _paste,
          child: const Text('Paste from clipboard'),
        ),
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy ? null : _import,
          child: Text(_busy ? 'Reading…' : 'Continue'),
        ),
      ],
    );
  }
}

class McpSheet extends StatefulWidget {
  const McpSheet({
    super.key,
    required this.store,
    this.editing,
    this.drafts = const [],
  });

  final TernStore store;
  final McpStatus? editing;
  final List<ImportedServer> drafts;

  @override
  State<McpSheet> createState() => _McpSheetState();
}

class _McpSheetState extends State<McpSheet> {
  var _tab = 0;
  final _saved = <int>{};

  int get _count => widget.drafts.isEmpty ? 1 : widget.drafts.length;

  void _onSaved(int i) {
    setState(() => _saved.add(i));
    if (_saved.length == _count) {
      Navigator.pop(context);
    } else {
      // On to the next one still to review.
      setState(
        () => _tab = List.generate(
          _count,
          (j) => j,
        ).firstWhere((j) => !_saved.contains(j)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final drafts = widget.drafts;
    final title = switch ((widget.editing, drafts.length)) {
      (final e?, _) => 'Edit ${e.name}',
      (_, 0) => 'Add an MCP server',
      (_, 1) => 'Review pasted server',
      (_, final n) => 'Review $n pasted servers',
    };
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      clipBehavior: Clip.antiAlias,
      child: SizedBox(
        width: 640,
        height: 660,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 14, 8, 10),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      title,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: () => Navigator.pop(context),
                  ),
                ],
              ),
            ),
            if (drafts.length > 1)
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 14),
                child: Row(
                  children: [
                    for (final (i, d) in drafts.indexed)
                      TextButton.icon(
                        onPressed: () => setState(() => _tab = i),
                        icon: _saved.contains(i)
                            ? Icon(Icons.check, size: 15, color: c.ok)
                            : const SizedBox.shrink(),
                        label: Text(
                          d.name,
                          style: mono.copyWith(
                            fontSize: 12.5,
                            fontWeight: i == _tab
                                ? FontWeight.w600
                                : FontWeight.w400,
                            color: i == _tab ? c.text : c.muted,
                          ),
                        ),
                        style: TextButton.styleFrom(
                          backgroundColor: i == _tab ? c.selection : null,
                        ),
                      ),
                  ],
                ),
              ),
            const Hairline(),
            Expanded(
              child: IndexedStack(
                index: _tab,
                children: [
                  for (var i = 0; i < _count; i++)
                    McpForm(
                      key: ValueKey(i),
                      store: widget.store,
                      editing: widget.editing,
                      draft: drafts.isEmpty ? null : drafts[i],
                      onSaved: () => _onSaved(i),
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

/// A key/value row: env `NAME=value`, or header → secret name.
class _Pair {
  _Pair([String key = '', String value = ''])
    : key = TextEditingController(text: key),
      value = TextEditingController(text: value);

  final TextEditingController key;
  final TextEditingController value;

  void dispose() {
    key.dispose();
    value.dispose();
  }
}

/// The §5 form for one server: fields, missing secrets, test, tools, save.
class McpForm extends StatefulWidget {
  const McpForm({
    super.key,
    required this.store,
    this.editing,
    this.draft,
    required this.onSaved,
  });

  final TernStore store;
  final McpStatus? editing;
  final ImportedServer? draft;
  final VoidCallback onSaved;

  @override
  State<McpForm> createState() => _McpFormState();
}

class _McpFormState extends State<McpForm> {
  /// Null until chosen on a blank add.
  McpTransport? _transport;
  late final McpServerConfig _base;
  final _name = TextEditingController();
  final _command = TextEditingController();
  final _argInput = TextEditingController();
  final _cwd = TextEditingController();
  final _url = TextEditingController();
  final _bearer = TextEditingController();
  final _args = <String>[];
  final _env = <_Pair>[];
  final _headers = <_Pair>[];
  final _envSecrets = <String>[];
  var _approval = false;
  var _idempotent = false;

  /// Typed secret values, by name. Sent only with test and save.
  final _values = <String, TextEditingController>{};
  final _shown = <String>{};

  /// Secrets already set that the user chose to replace.
  final _changing = <String>{};
  var _known = <String, SecretStatus>{};
  late final Set<String> _disabled;

  McpVerifyResult? _result;
  String? _error;
  var _busy = false;
  final _scroll = ScrollController();

  TernClient get _client => widget.store.client;

  @override
  void initState() {
    super.initState();
    final from = widget.editing?.config ?? widget.draft?.config;
    _base = from ?? const McpServerConfig();
    if (from != null) {
      _transport = from.transport;
      _name.text = widget.editing?.name ?? widget.draft!.name;
      _command.text = from.command ?? '';
      _args.addAll(from.args);
      _cwd.text = from.cwd ?? '';
      _url.text = from.url ?? '';
      _bearer.text = from.bearerTokenSecret ?? '';
      for (final MapEntry(:key, :value) in from.env.entries) {
        _env.add(_Pair(key, value));
      }
      for (final MapEntry(:key, :value) in from.headerSecrets.entries) {
        _headers.add(_Pair(key, value));
      }
      _envSecrets.addAll(from.envSecrets);
      _approval = from.requiresApproval;
      _idempotent = from.idempotent;
    }
    _disabled = {...?from?.disabledTools};
    for (final MapEntry(:key, :value)
        in (widget.draft?.secrets ?? {}).entries) {
      if (value != null) _valueFor(key).text = value;
    }
    _loadSecrets();
  }

  Future<void> _loadSecrets() async {
    try {
      final list = await _client.secrets();
      if (mounted) setState(() => _known = {for (final s in list) s.name: s});
    } on TernException {
      // Then every secret shows as a field to fill in.
    }
  }

  @override
  void dispose() {
    _scroll.dispose();
    for (final t in [_name, _command, _argInput, _cwd, _url, _bearer]) {
      t.dispose();
    }
    for (final p in [..._env, ..._headers]) {
      p.dispose();
    }
    for (final v in _values.values) {
      v.dispose();
    }
    super.dispose();
  }

  TextEditingController _valueFor(String name) =>
      _values[name] ??= TextEditingController();

  bool get _stdioBlocked =>
      widget.store.remoteCore && _transport == McpTransport.stdio;

  McpServerConfig get _config {
    String? blank(TextEditingController t) =>
        t.text.trim().isEmpty ? null : t.text.trim();
    final pending = _argInput.text.trim();
    return McpServerConfig(
      transport: _transport ?? McpTransport.stdio,
      enabled: _base.enabled,
      command: blank(_command),
      args: [..._args, if (pending.isNotEmpty) pending],
      env: {
        for (final p in _env)
          if (p.key.text.trim().isNotEmpty) p.key.text.trim(): p.value.text,
      },
      cwd: blank(_cwd),
      url: blank(_url),
      envSecrets: _envSecrets,
      bearerTokenSecret: blank(_bearer),
      headerSecrets: {
        for (final p in _headers)
          if (p.key.text.trim().isNotEmpty && p.value.text.trim().isNotEmpty)
            p.key.text.trim(): p.value.text.trim(),
      },
      disabledTools: _disabled.toList()..sort(),
      idempotent: _idempotent,
      requiresApproval: _approval,
      effects: _base.effects,
    );
  }

  /// Every secret this config needs, once each.
  List<String> get _needed => {..._config.secretNames}.toList();

  bool _isSet(String name) => _known[name]?.isSet ?? false;

  /// Values to send: typed ones for secrets this config uses.
  Map<String, String> get _typed => {
    for (final n in _needed)
      if (_values[n]?.text.trim() case final v? when v.isNotEmpty) n: v,
  };

  List<String> get _missing => [
    for (final n in _needed)
      if (!_isSet(n) && !_typed.containsKey(n)) n,
  ];

  String? _checkName() {
    final n = _name.text.trim();
    if (n.isEmpty) return 'Give it a name.';
    if (!_serverName.hasMatch(n)) {
      return 'Names are 1–24 characters from a-z, 0-9, _ and -.';
    }
    return null;
  }

  Future<void> _test() async {
    if (_checkName() case final e?) return setState(() => _error = e);
    if (_missing.isNotEmpty) {
      setState(
        () => _error =
            'Enter ${_missing.join(', ')} first, or save without testing.',
      );
      return _reveal();
    }
    setState(() {
      _busy = true;
      _error = null;
      _result = null;
    });
    try {
      final r = await _client.verifyMcp(
        _name.text.trim(),
        _config,
        secrets: _typed,
      );
      if (mounted) setState(() => _result = r);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
      _reveal();
    }
  }

  /// Scroll to the result or error, which sit below the fields.
  void _reveal() => WidgetsBinding.instance.addPostFrameCallback((_) {
    if (!_scroll.hasClients) return;
    _scroll.animateTo(
      _scroll.position.maxScrollExtent,
      duration: const Duration(milliseconds: 200),
      curve: Curves.easeOut,
    );
  });

  Future<void> _save() async {
    if (_checkName() case final e?) return setState(() => _error = e);
    setState(() {
      _busy = true;
      _error = null;
    });
    final name = _name.text.trim();
    try {
      if (widget.editing case final old?) {
        await _client.updateMcp(
          old.name,
          name: name == old.name ? null : name,
          config: _config,
          secrets: _typed,
        );
      } else {
        await _client.addMcp(name, _config, secrets: _typed);
      }
      await widget.store.loadMcp();
      if (widget.editing != null && name != widget.editing!.name) {
        widget.store.openMcp(name);
      }
      if (mounted) widget.onSaved();
    } on TernException catch (e) {
      if (mounted) {
        setState(
          () => _error = e.status == 409
              ? 'There is already a server called $name. Pick another name.'
              : e.message,
        );
      }
      _reveal();
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Move an env row into `env_secrets`, carrying its value to the
  /// secret field.
  void _makeSecret(_Pair p) {
    final key = p.key.text.trim();
    if (!secretName.hasMatch(key)) {
      return setState(
        () => _error =
            '"$key" can\'t be a secret name: use letters, digits '
            'and _.',
      );
    }
    setState(() {
      _env.remove(p);
      if (!_envSecrets.contains(key)) _envSecrets.add(key);
      if (p.value.text.isNotEmpty) _valueFor(key).text = p.value.text;
      _changing.add(key);
      _error = null;
    });
    p.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_transport == null) return _choose(context);
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final stdio = _transport == McpTransport.stdio;
    final r = _result;
    final renamed =
        widget.editing != null && _name.text.trim() != widget.editing!.name;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(
          child: SingleChildScrollView(
            controller: _scroll,
            padding: const EdgeInsets.fromLTRB(20, 16, 20, 16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (final w in widget.draft?.warnings ?? const <String>[])
                  _Note(w, color: c.warn),
                if (_stdioBlocked) _Note(remoteStdioOff, color: c.error),
                TextField(
                  key: const ValueKey('mcp-name'),
                  controller: _name,
                  autocorrect: false,
                  enableSuggestions: false,
                  onChanged: (_) => setState(() {}),
                  style: mono.copyWith(fontSize: 13),
                  decoration: _box(context, label: 'Name', hint: 'github'),
                ),
                if (renamed)
                  _Note(
                    'Renaming: “always allow” choices for '
                    'mcp__${widget.editing!.name}__* won\'t carry over.',
                    color: c.warn,
                  ),
                const SizedBox(height: 12),
                if (stdio)
                  ..._stdioFields(context)
                else
                  ..._httpFields(context),
                const SizedBox(height: 16),
                Text('SECRETS', style: labels),
                const SizedBox(height: 6),
                if (_needed.isEmpty)
                  Text(
                    stdio
                        ? 'None. Use “Make secret” on a variable that holds a key.'
                        : 'None.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                for (final n in _needed) _secretRow(context, n),
                const SizedBox(height: 12),
                _check(
                  'Ask before each call',
                  _approval,
                  (v) => setState(() => _approval = v),
                ),
                _check(
                  'Safe to retry (calls have no side effects if repeated)',
                  _idempotent,
                  (v) => setState(() => _idempotent = v),
                ),
                if (_error case final e?) _Note(e, color: c.error),
                if (r != null) ..._resultView(context, r),
              ],
            ),
          ),
        ),
        const Hairline(),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 10, 16, 12),
          child: Row(
            children: [
              if (widget.editing == null && widget.draft == null)
                TextButton(
                  onPressed: _busy
                      ? null
                      : () => setState(() {
                          _transport = null;
                          _result = null;
                          _error = null;
                        }),
                  child: const Text('Back'),
                ),
              Expanded(
                child: Wrap(
                  alignment: WrapAlignment.end,
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    if (r?.ok ?? false) ...[
                      TextButton(
                        onPressed: _busy || _stdioBlocked ? null : _test,
                        child: const Text('Test again'),
                      ),
                      FilledButton(
                        onPressed: _busy || _stdioBlocked ? null : _save,
                        child: Text(_busy ? 'Saving…' : 'Save'),
                      ),
                    ] else ...[
                      TextButton(
                        onPressed: _busy || _stdioBlocked ? null : _save,
                        child: const Text('Save without testing'),
                      ),
                      FilledButton(
                        onPressed: _busy || _stdioBlocked ? null : _test,
                        child: Text(_busy ? 'Testing…' : 'Test connection'),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _choose(BuildContext context) {
    final remote = widget.store.remoteCore;
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        _TransportCard(
          icon: Icons.terminal,
          title: 'Local command',
          blurb: remote
              ? remoteStdioOff
              : 'Runs a program on this machine (stdio), e.g. npx or uvx.',
          onTap: remote
              ? null
              : () => setState(() => _transport = McpTransport.stdio),
        ),
        _TransportCard(
          icon: Icons.public,
          title: 'Remote URL',
          blurb: 'Connects to a server over streamable HTTP.',
          onTap: () => setState(() => _transport = McpTransport.http),
        ),
      ],
    );
  }

  List<Widget> _stdioFields(BuildContext context) {
    final c = context.colors;
    return [
      TextField(
        key: const ValueKey('mcp-command'),
        controller: _command,
        autocorrect: false,
        enableSuggestions: false,
        style: mono.copyWith(fontSize: 13),
        decoration: _box(context, label: 'Command', hint: 'npx'),
      ),
      const SizedBox(height: 10),
      Wrap(
        spacing: 6,
        runSpacing: 6,
        crossAxisAlignment: WrapCrossAlignment.center,
        children: [
          for (final (i, a) in _args.indexed)
            InputChip(
              label: Text(a, style: mono.copyWith(fontSize: 12)),
              visualDensity: VisualDensity.compact,
              onDeleted: () => setState(() => _args.removeAt(i)),
            ),
          SizedBox(
            width: 220,
            child: TextField(
              key: const ValueKey('mcp-arg'),
              controller: _argInput,
              autocorrect: false,
              enableSuggestions: false,
              style: mono.copyWith(fontSize: 12.5),
              decoration: _box(context, hint: 'Argument, then ⏎'),
              onSubmitted: (v) {
                if (v.trim().isEmpty) return;
                setState(() {
                  _args.add(v.trim());
                  _argInput.clear();
                });
              },
            ),
          ),
        ],
      ),
      const SizedBox(height: 14),
      Text('ENVIRONMENT', style: Theme.of(context).textTheme.labelSmall),
      const SizedBox(height: 6),
      for (final p in _env)
        _pairRow(
          context,
          p,
          keyHint: 'NAME',
          valueHint: 'value',
          action: TextButton(
            onPressed: () => _makeSecret(p),
            style: TextButton.styleFrom(foregroundColor: c.accent),
            child: const Text('Make secret'),
          ),
          onRemove: () => setState(() {
            _env.remove(p);
            p.dispose();
          }),
        ),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          onPressed: () => setState(() => _env.add(_Pair())),
          icon: const Icon(Icons.add, size: 15),
          label: const Text('Add variable'),
        ),
      ),
      const SizedBox(height: 6),
      TextField(
        controller: _cwd,
        autocorrect: false,
        enableSuggestions: false,
        style: mono.copyWith(fontSize: 13),
        decoration: _box(
          context,
          label: 'Working directory (optional)',
          hint: '~/code/project',
        ),
      ),
    ];
  }

  List<Widget> _httpFields(BuildContext context) => [
    TextField(
      key: const ValueKey('mcp-url'),
      controller: _url,
      autocorrect: false,
      enableSuggestions: false,
      style: mono.copyWith(fontSize: 13),
      decoration: _box(
        context,
        label: 'URL',
        hint: 'https://mcp.example.com/mcp',
      ),
    ),
    const SizedBox(height: 10),
    TextField(
      key: const ValueKey('mcp-bearer'),
      controller: _bearer,
      autocorrect: false,
      enableSuggestions: false,
      onChanged: (_) => setState(() {}),
      style: mono.copyWith(fontSize: 13),
      decoration: _box(
        context,
        label: 'Bearer token secret (optional)',
        hint: 'LINEAR_TOKEN',
      ),
    ),
    const SizedBox(height: 14),
    Text('HEADERS', style: Theme.of(context).textTheme.labelSmall),
    const SizedBox(height: 6),
    for (final p in _headers)
      _pairRow(
        context,
        p,
        keyHint: 'X-API-Key',
        valueHint: 'SECRET_NAME',
        onChanged: () => setState(() {}),
        onRemove: () => setState(() {
          _headers.remove(p);
          p.dispose();
        }),
      ),
    Align(
      alignment: Alignment.centerLeft,
      child: TextButton.icon(
        onPressed: () => setState(() => _headers.add(_Pair())),
        icon: const Icon(Icons.add, size: 15),
        label: const Text('Add header secret'),
      ),
    ),
  ];

  Widget _pairRow(
    BuildContext context,
    _Pair p, {
    required String keyHint,
    required String valueHint,
    Widget? action,
    VoidCallback? onChanged,
    required VoidCallback onRemove,
  }) => Padding(
    padding: const EdgeInsets.only(bottom: 6),
    child: Row(
      children: [
        Expanded(
          flex: 2,
          child: TextField(
            controller: p.key,
            autocorrect: false,
            enableSuggestions: false,
            onChanged: (_) => onChanged?.call(),
            style: mono.copyWith(fontSize: 12.5),
            decoration: _box(context, hint: keyHint),
          ),
        ),
        const SizedBox(width: 6),
        Expanded(
          flex: 3,
          child: TextField(
            controller: p.value,
            autocorrect: false,
            enableSuggestions: false,
            onChanged: (_) => onChanged?.call(),
            style: mono.copyWith(fontSize: 12.5),
            decoration: _box(context, hint: valueHint),
          ),
        ),
        ?action,
        QuietIconButton(
          icon: Icons.close,
          tooltip: 'Remove',
          onPressed: onRemove,
        ),
      ],
    ),
  );

  /// A secret that's set shows where from and can be replaced; otherwise
  /// a masked field with Show and Paste, like the provider key field.
  Widget _secretRow(BuildContext context, String name) {
    final c = context.colors;
    final known = _known[name];
    final removable =
        _envSecrets.contains(name) &&
        _transport == McpTransport.stdio &&
        widget.editing == null;
    final remove = removable
        ? QuietIconButton(
            icon: Icons.close,
            tooltip: 'Stop passing $name',
            size: 16,
            onPressed: () => setState(() => _envSecrets.remove(name)),
          )
        : null;
    if (known != null && known.isSet && !_changing.contains(name)) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 2),
        child: Row(
          children: [
            Text(name, style: mono.copyWith(fontSize: 12.5)),
            const SizedBox(width: 8),
            Icon(Icons.check, size: 14, color: c.ok),
            const SizedBox(width: 4),
            Text(
              'set (${known.inEnv ? 'env' : 'file'})',
              style: TextStyle(fontSize: 12, color: c.ok),
            ),
            const Spacer(),
            TextButton(
              onPressed: () => setState(() => _changing.add(name)),
              child: const Text('Change'),
            ),
            ?remove,
          ],
        ),
      );
    }
    final show = _shown.contains(name);
    final missing = !_isSet(name);
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        children: [
          Expanded(
            child: TextField(
              key: ValueKey('mcp-secret-$name'),
              controller: _valueFor(name),
              obscureText: !show,
              autocorrect: false,
              enableSuggestions: false,
              enableIMEPersonalizedLearning: false,
              onChanged: (_) => setState(() {}),
              style: mono.copyWith(fontSize: 13),
              decoration: _box(
                context,
                label: missing ? '$name · not set' : name,
                hint: 'Paste the value',
                suffix: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    IconButton(
                      tooltip: show ? 'Hide' : 'Show',
                      icon: Icon(
                        show ? Icons.visibility_off : Icons.visibility,
                        size: 17,
                        color: c.muted,
                      ),
                      onPressed: () => setState(
                        () => show ? _shown.remove(name) : _shown.add(name),
                      ),
                    ),
                    TextButton(
                      onPressed: () async {
                        final d = await Clipboard.getData(Clipboard.kTextPlain);
                        if (d?.text case final t?) {
                          setState(() => _valueFor(name).text = t.trim());
                        }
                      },
                      child: const Text('Paste'),
                    ),
                  ],
                ),
              ),
            ),
          ),
          ?remove,
        ],
      ),
    );
  }

  Widget _check(String label, bool value, ValueChanged<bool> onChanged) => Row(
    children: [
      Checkbox(
        value: value,
        visualDensity: VisualDensity.compact,
        onChanged: (v) => onChanged(v!),
      ),
      Expanded(child: Text(label, style: const TextStyle(fontSize: 12.5))),
    ],
  );

  List<Widget> _resultView(BuildContext context, McpVerifyResult r) {
    final c = context.colors;
    if (r.error case final e?) {
      return [
        const SizedBox(height: 10),
        Row(
          children: [
            Icon(Icons.error_outline, size: 16, color: c.error),
            const SizedBox(width: 6),
            Text(
              e.label,
              style: TextStyle(
                fontSize: 13,
                fontWeight: FontWeight.w600,
                color: c.error,
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        SelectableText(e.message, style: const TextStyle(fontSize: 12.5)),
        if (e.stderrTail.isNotEmpty) ...[
          const SizedBox(height: 6),
          Row(
            children: [
              Text('STDERR', style: Theme.of(context).textTheme.labelSmall),
              const Spacer(),
              CopyButton(
                text: () => e.stderrTail.join('\n'),
                tooltip: 'Copy stderr',
              ),
            ],
          ),
          McpLogs(e.stderrTail),
        ],
      ];
    }
    final v = r.verified!;
    final on = v.tools.where((t) => !_disabled.contains(t.remote)).length;
    return [
      const SizedBox(height: 10),
      Row(
        children: [
          Icon(Icons.check_circle, size: 16, color: c.ok),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              'Connected — ${[?v.serverInfo, '${v.tools.length} tools'].join(', ')}',
              style: TextStyle(fontSize: 12.5, color: c.ok),
            ),
          ),
        ],
      ),
      if (v.tools.isNotEmpty) ...[
        const SizedBox(height: 10),
        Text(
          'TOOLS · $on of ${v.tools.length} on',
          style: Theme.of(context).textTheme.labelSmall,
        ),
        for (final t in v.tools)
          CheckboxListTile(
            value: !_disabled.contains(t.remote),
            dense: true,
            contentPadding: EdgeInsets.zero,
            controlAffinity: ListTileControlAffinity.leading,
            title: Text(t.remote, style: mono.copyWith(fontSize: 12.5)),
            subtitle: t.description.isEmpty
                ? null
                : Text(
                    t.description.split('\n').first,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
            onChanged: (v) => setState(
              () => v! ? _disabled.remove(t.remote) : _disabled.add(t.remote),
            ),
          ),
      ],
    ];
  }
}

class _Note extends StatelessWidget {
  const _Note(this.text, {required this.color});

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 6),
    child: Text(text, style: TextStyle(fontSize: 12.5, color: color)),
  );
}

class _TransportCard extends StatelessWidget {
  const _TransportCard({
    required this.icon,
    required this.title,
    required this.blurb,
    required this.onTap,
  });

  final IconData icon;
  final String title;
  final String blurb;

  /// Null when it can't be chosen here.
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final off = onTap == null;
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(10),
        child: Opacity(
          opacity: off ? 0.6 : 1,
          child: Container(
            padding: const EdgeInsets.all(14),
            decoration: BoxDecoration(
              border: Border.all(color: c.hairline),
              borderRadius: BorderRadius.circular(10),
            ),
            child: Row(
              children: [
                Icon(icon, size: 20, color: off ? c.muted : c.accent),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        title,
                        style: const TextStyle(
                          fontSize: 13.5,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                      const SizedBox(height: 2),
                      Text(
                        blurb,
                        style: TextStyle(
                          fontSize: 12,
                          color: off ? c.warn : c.muted,
                        ),
                      ),
                    ],
                  ),
                ),
                if (!off) Icon(Icons.chevron_right, size: 18, color: c.muted),
              ],
            ),
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
