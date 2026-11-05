import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'underline_tabs.dart';
import 'agent_ap3.dart';
import 'agent_starters.dart';
import 'channel_view.dart' show slugFor;
import 'manage.dart';
import 'model_chain_editor.dart';
import 'model_picker.dart' show ModelList;
import 'secrets.dart' show showSecretsDialog;
import 'widgets.dart';
import 'workspace_ui.dart';

/// Create an agent (`agent` null) or edit one: the sheet of agent profiles
/// §7.2. A template only fills the sheet.
Future<void> showAgentSheet(
  BuildContext context,
  TernStore store, {
  Agent? agent,
}) => showDialog<void>(
  context: context,
  builder: (_) => AgentSheet(store: store, editing: agent),
);

/// Built-in tools are the tools that don't come from a connector (`lua__`)
/// or an MCP server (`mcp__`).
bool isBuiltin(String tool) =>
    !tool.startsWith('lua__') && !tool.startsWith('mcp__');

/// The connector a `lua__<connector>__<tool>` tool belongs to.
String? connectorOf(String tool) =>
    tool.startsWith('lua__') ? tool.substring(5).split('__').firstOrNull : null;

/// What every agent keeps, whatever its scope (§1.2).
const systemTools = {
  'channel_read',
  'channel_post',
  'history_search',
  'secret_request',
  'memory_save',
  'memory_search',
  'profile_edit',
  'session_search',
};

/// `tools: "read_only"` (the core's list).
const readOnlyTools = {
  'read_file',
  'list_dir',
  'search_files',
  'http_fetch',
  'memory_search',
  'session_search',
  'skill_read',
};

/// Whether agent `a`'s scope lets it use `tool` (a missing scope: all).
bool agentAllows(Agent a, String tool) {
  final s = a.tools;
  if (s == null || systemTools.contains(tool)) return true;
  if (connectorOf(tool) case final c?) return s.connectors.allows(c);
  if (tool.startsWith('mcp__')) {
    final server = tool.substring(5).split('__').firstOrNull ?? '';
    return s.mcp.allows(server);
  }
  return s.builtins.all ||
      (s.builtins.readOnly && readOnlyTools.contains(tool)) ||
      s.builtins.names.contains(tool);
}

class AgentSheet extends StatefulWidget {
  const AgentSheet({super.key, required this.store, this.editing});

  final TernStore store;
  final Agent? editing;

  @override
  State<AgentSheet> createState() => _AgentSheetState();
}

class _AgentSheetState extends State<AgentSheet> {
  late final _name = TextEditingController(text: widget.editing?.name);
  final _persona = TextEditingController();

  /// The model chain (T6a): empty = the default model.
  late List<String> _model = [...?widget.editing?.model];
  late List<String> _subagentModel = [...?widget.editing?.subagentModel];
  String? _personaBefore;
  String? _system;
  List<AgentTemplate> _templates = [];
  String? _template;
  ({String cron, String prompt})? _routine;
  var _addRoutine = true;
  late String _glyph =
      widget.editing?.avatar?.glyph ?? AgentAvatar.glyphs.first;
  late String _color =
      widget.editing?.avatar?.color ??
      AgentAvatar.colorNames[DateTime.now().millisecond % 8];
  late String _memory = widget.editing?.memory ?? 'private';

  /// Its runs read any site without asking, even after untrusted content.
  late bool _browse = widget.editing?.egress == 'allow';

  /// Who it may hand work to, by id; null = unchanged from the agent's.
  List<String>? _delegates;

  /// Its autonomy ceiling (`readonly`..`yolo`); null = none.
  late String? _autonomy = widget.editing?.autonomy;

  /// The NAME of the secret holding its own Telegram bot's token; null =
  /// no bot of its own.
  late String? _bot = widget.editing?.telegramSecret;

  /// The user's secrets (names and status; never values), for the bot field.
  List<SecretStatus>? _secrets;
  String? _secretsError;

  /// The scope being edited; null = everything (the default agent's).
  late ToolScope? _tools = widget.editing == null
      ? _narrow()
      : widget.editing!.tools;
  late final _slugCtl = TextEditingController(text: widget.editing?.slug);
  String? _error;
  var _busy = false;

  bool get _creating => widget.editing == null;
  bool get _isDefault => widget.editing?.isDefault ?? false;

  List<String> get _builtins => [
    for (final t in widget.store.info?.tools ?? const [])
      if (isBuiltin(t.name) && !systemTools.contains(t.name)) t.name,
  ];

  List<String> get _connectors => {
    for (final t in widget.store.info?.tools ?? const []) ?connectorOf(t.name),
  }.toList()..sort();

  /// A new agent's default: built-in tools except `shell`, no connectors,
  /// no MCP servers, all skills.
  ToolScope _narrow() => ToolScope(
    builtins: Pick.names([
      for (final t in _builtins)
        if (t != 'shell') t,
    ]),
    connectors: const Pick.names([]),
    mcp: const Pick.names([]),
  );

  @override
  void initState() {
    super.initState();
    _name.addListener(() => setState(() {}));
    _slugCtl.addListener(() => setState(() {}));
    _load();
  }

  Future<void> _loadSecrets() async {
    try {
      final list = await widget.store.client.secrets();
      if (mounted) {
        setState(() {
          _secrets = list;
          _secretsError = null;
        });
      }
    } on TernException catch (e) {
      if (mounted) setState(() => _secretsError = e.message);
    }
  }

  Future<void> _load() async {
    final client = widget.store.client;
    if (widget.editing case final a? when !a.isDefault) _loadSecrets();
    try {
      if (widget.editing case final a?) {
        final p = await client.persona(a.id);
        if (!mounted) return;
        setState(() {
          _persona.text = p.persona;
          _personaBefore = p.persona;
          _system = p.systemSections;
        });
      } else {
        final t = await client.agentTemplates();
        if (!mounted) return;
        setState(() => _templates = t);
        _apply(t.where((x) => x.id == 'blank').firstOrNull);
      }
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  /// Fill the sheet from a template; never a blank page.
  void _apply(AgentTemplate? t) {
    if (t == null) return;
    setState(() {
      _template = t.id;
      if (t.name.isNotEmpty) _name.text = t.name;
      final name = _name.text.trim().isEmpty ? 'this agent' : _name.text.trim();
      _persona.text = t.persona.replaceAll('{name}', name);
      _memory = t.memory;
      // The starter's ceiling, pre-selected in the picker below; changeable.
      _autonomy = t.autonomyLevel?.wire;
      _tools = t.tools ?? _narrow();
      _routine = t.routine;
      if (t.avatar case final a?) {
        _glyph = a.glyph;
        _color = a.color;
      }
    });
  }

  /// A chain as the API takes it: null, one model, or a list.
  static Object? _wire(List<String> chain) => switch (chain) {
    [] => null,
    [final one] => one,
    _ => chain,
  };

  Future<void> _save() async {
    final store = widget.store;
    if (chainBlocked(store, _model, saved: [...?widget.editing?.model]) ||
        chainBlocked(
          store,
          _subagentModel,
          saved: [...?widget.editing?.subagentModel],
        )) {
      setState(
        () => _error =
            'A model in a fallback list needs fixing first (see the red '
            'rows under Model).',
      );
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    final avatar = AvatarSpec(glyph: _glyph, color: _color);
    final String? err;
    var botChanged = false;
    if (widget.editing case final a?) {
      final model = _model;
      // The default agent's fallbacks changed: written to `default_model`.
      final defaultChain =
          _isDefault && model.skip(1).join(',') != a.model.skip(1).join(',');
      final fields = <String, Object?>{
        if (_name.text.trim() != a.name) 'name': _name.text.trim(),
        if (!_isDefault && _slugCtl.text.trim() != a.slug)
          'slug': _slugCtl.text.trim().replaceFirst('@', ''),
        'avatar': avatar.toJson(),
        // The default agent's model is `default_model`: a new primary is
        // sent as one model (the daemon keeps that setting's fallbacks);
        // edited fallbacks go to `default_model` itself, below.
        if (model.join(',') != a.model.join(',') && !defaultChain)
          'model': _isDefault ? model.firstOrNull : _wire(model),
        if (_subagentModel.join(',') != a.subagentModel.join(','))
          'subagent_model': _wire(_subagentModel),
        if (!_isDefault) 'memory': _memory,
        if (_browse != (a.egress == 'allow'))
          'egress': _browse ? 'allow' : null,
        if (!_isDefault || _tools != null) 'tools': _tools?.toJson(),
        'delegates': ?_delegates,
        // `null` removes the ceiling: sent, not left out.
        if (_autonomy != a.autonomy) 'autonomy': _autonomy,
        if (!_isDefault && _bot != a.telegramSecret)
          'telegram': _bot == null ? null : {'bot_token_secret': _bot},
      };
      botChanged = !_isDefault && _bot != a.telegramSecret;
      err = await store.updateAgent(
        a,
        fields: fields,
        persona: _persona.text.trim() == _personaBefore?.trim()
            ? null
            : _persona.text,
        defaultChain: defaultChain ? model : null,
      );
    } else {
      err = await store.createAgent(
        name: _name.text.trim(),
        avatar: avatar,
        persona: _persona.text,
        model: _model,
        subagentModel: _subagentModel,
        tools: _tools,
        memory: _memory,
        egress: _browse ? 'allow' : null,
        autonomy: _autonomy,
        template: _template,
        routine: _addRoutine ? _routine : null,
      );
    }
    if (!mounted) return;
    if (err == null) {
      final messenger = ScaffoldMessenger.maybeOf(context);
      Navigator.of(context).pop();
      if (botChanged) {
        messenger?.showSnackBar(
          const SnackBar(
            content: Text(
              'The bot change applies when the daemon restarts '
              '(Restart daemon, at the bottom of the sidebar).',
            ),
          ),
        );
      }
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  Widget _section(String label) => Padding(
    padding: const EdgeInsets.only(top: 14, bottom: 4),
    child: Text(label, style: Theme.of(context).textTheme.labelSmall),
  );

  /// One line saying what Advanced holds right now, so a collapsed section
  /// still shows what will be saved.
  String get _advancedSummary {
    final tools = _tools;
    final n = tools == null
        ? 0
        : _builtins
              .where(
                (t) => tools.builtins.all || tools.builtins.names.contains(t),
              )
              .length;
    final toolsText = tools == null
        ? 'all tools'
        : tools.builtins.readOnly
        ? 'read-only tools'
        : '$n built-in ${n == 1 ? 'tool' : 'tools'}';
    final ceiling = AutonomyLevel.parse(_autonomy)?.label.toLowerCase();
    return [
      toolsText,
      '$_memory memory',
      ceiling == null ? 'no ceiling' : '$ceiling ceiling',
      if (_bot != null) 'own bot',
    ].join(' · ');
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final editing = widget.editing;
    final slug = _isDefault
        ? 'tern'
        : editing != null
        ? _slugCtl.text.trim().replaceFirst('@', '')
        : slugFor(_name.text);
    final ok = _name.text.trim().isNotEmpty && slug.isNotEmpty && !_busy;
    final input = InputDecoration(
      isDense: true,
      border: OutlineInputBorder(borderRadius: BorderRadius.circular(8)),
    );
    final small = Theme.of(context).textTheme.bodySmall;
    final flat = Theme.of(context).copyWith(dividerColor: Colors.transparent);
    return Dialog(
      insetPadding: const EdgeInsets.symmetric(horizontal: 24, vertical: 24),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520, maxHeight: 720),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 16, 20, 8),
              child: Row(
                children: [
                  AgentAvatar(
                    name: editing?.id ?? slug,
                    size: 30,
                    glyph: _glyph,
                    color: _color,
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          editing == null
                              ? 'New agent'
                              : 'Edit ${editing.name}',
                          style: const TextStyle(
                            fontSize: 16,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        Text(
                          _isDefault
                              ? 'Your default agent. Home is its chat.'
                              : editing == null
                              ? 'Pick a starter or start blank, name it, create.'
                              : 'Changes apply from its next run.',
                          style: small,
                        ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
            Flexible(
              child: SingleChildScrollView(
                padding: const EdgeInsets.fromLTRB(20, 6, 20, 8),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    if (_creating && _templates.isNotEmpty) ...[
                      StarterField(
                        templates: _templates,
                        current: _template,
                        onPicked: _apply,
                      ),
                      const SizedBox(height: 12),
                    ],
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Expanded(
                          flex: 5,
                          child: TextField(
                            key: const Key('agent-name'),
                            controller: _name,
                            autofocus: _creating,
                            decoration: input.copyWith(
                              labelText: 'Name',
                              hintText: 'Research',
                              suffixText: slug.isEmpty || editing != null
                                  ? null
                                  : '@$slug',
                            ),
                          ),
                        ),
                        if (editing != null && !_isDefault) ...[
                          const SizedBox(width: 10),
                          Expanded(
                            flex: 4,
                            child: TextField(
                              key: const Key('agent-slug'),
                              controller: _slugCtl,
                              decoration: input.copyWith(
                                labelText: '@name',
                                prefixText: '@',
                              ),
                            ),
                          ),
                        ],
                      ],
                    ),
                    if (editing != null && !_isDefault)
                      Padding(
                        padding: const EdgeInsets.only(top: 4, left: 2),
                        child: Text(
                          'The @name is for @mentions and its chat '
                          '(#dm-$slug). Its history keeps pointing at it.',
                          style: small,
                        ),
                      ),
                    const SizedBox(height: 12),
                    TextField(
                      key: const Key('agent-persona'),
                      controller: _persona,
                      minLines: 4,
                      maxLines: 8,
                      style: const TextStyle(fontSize: 13),
                      decoration: input.copyWith(
                        labelText: 'Persona',
                        alignLabelWithHint: true,
                        hintText: 'Who it is, its job, its tone.',
                      ),
                    ),
                    Theme(
                      data: flat,
                      child: ExpansionTile(
                        tilePadding: EdgeInsets.zero,
                        dense: true,
                        visualDensity: VisualDensity.compact,
                        title: Text(
                          'tern adds these to every agent',
                          style: TextStyle(fontSize: 12, color: c.muted),
                        ),
                        children: [
                          Container(
                            width: double.infinity,
                            constraints: const BoxConstraints(maxHeight: 180),
                            padding: const EdgeInsets.all(10),
                            decoration: BoxDecoration(
                              color: c.sidebar,
                              borderRadius: BorderRadius.circular(8),
                            ),
                            child: SingleChildScrollView(
                              child: SelectableText(
                                _system ??
                                    'Safety, secrets, untrusted content, '
                                        'memory, the time, the workspace, '
                                        'sub-agents and who your other agents '
                                        'are. A persona changes who the agent '
                                        'is, not these rules.',
                                style: mono.copyWith(
                                  fontSize: 11.5,
                                  color: c.muted,
                                ),
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                    AgentModelField(
                      key: const Key('agent-model'),
                      label: 'Model',
                      store: widget.store,
                      chain: _model,
                      allowDefault: !_isDefault,
                      defaultLabel: 'Default',
                      onChanged: (v) => setState(() => _model = v),
                    ),
                    if (_isDefault)
                      Padding(
                        padding: const EdgeInsets.only(top: 4),
                        child: Text(
                          'Your default model: other agents without a model '
                          'of their own use it too.',
                          style: small,
                        ),
                      ),
                    if (_routine case final r? when _creating)
                      CheckboxListTile(
                        contentPadding: EdgeInsets.zero,
                        dense: true,
                        visualDensity: VisualDensity.compact,
                        controlAffinity: ListTileControlAffinity.leading,
                        value: _addRoutine,
                        onChanged: (v) =>
                            setState(() => _addRoutine = v ?? false),
                        title: Text(
                          'Add its routine: ${r.prompt}',
                          style: const TextStyle(fontSize: 13),
                        ),
                        subtitle: Text(
                          r.cron,
                          style: mono.copyWith(fontSize: 11.5),
                        ),
                      ),
                    Theme(
                      data: flat,
                      child: ExpansionTile(
                        key: const Key('advanced-toggle'),
                        tilePadding: EdgeInsets.zero,
                        childrenPadding: EdgeInsets.zero,
                        dense: true,
                        visualDensity: VisualDensity.compact,
                        expandedCrossAxisAlignment: CrossAxisAlignment.stretch,
                        title: const Text(
                          'Advanced',
                          style: TextStyle(
                            fontSize: 13,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        subtitle: Text(
                          _advancedSummary,
                          key: const Key('advanced-summary'),
                          style: TextStyle(fontSize: 11.5, color: c.muted),
                        ),
                        children: [
                          _section('Avatar'),
                          _AvatarPicker(
                            glyph: _glyph,
                            color: _color,
                            onGlyph: (g) => setState(() => _glyph = g),
                            onColor: (v) => setState(() => _color = v),
                          ),
                          _section('Sub-agent model'),
                          AgentModelField(
                            key: const Key('agent-subagent-model'),
                            store: widget.store,
                            chain: _subagentModel,
                            allowDefault: true,
                            defaultLabel: 'Default for sub-agents',
                            defaultDetail:
                                "sub-agents' configured model, else this "
                                "agent's",
                            onChanged: (v) =>
                                setState(() => _subagentModel = v),
                          ),
                          _section('Tools'),
                          _ToolScopeEditor(
                            scope: _tools,
                            builtins: _builtins,
                            connectors: _connectors,
                            mcp: widget.store.mcpServers,
                            onChanged: (t) => setState(() => _tools = t),
                          ),
                          if (widget.editing case final a?)
                            if (widget.store.workspaces case final api?) ...[
                              _section('Folder'),
                              AgentFolderField(api: api, agentId: a.id),
                            ],
                          _section('Memory'),
                          RadioGroup<String>(
                            groupValue: _memory,
                            onChanged: (v) {
                              if (v != null && !_isDefault) {
                                setState(() => _memory = v);
                              }
                            },
                            child: Row(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Expanded(
                                  child: _MemoryChoice(
                                    value: 'private',
                                    title: 'Private',
                                    line:
                                        'Its own notes and memories. What it '
                                        'knows about you (user.md) is shared.',
                                    enabled: !_isDefault,
                                  ),
                                ),
                                Expanded(
                                  child: _MemoryChoice(
                                    value: 'shared',
                                    title: 'Shared',
                                    line:
                                        'Reads and writes the memory tern '
                                        'uses.',
                                    enabled: !_isDefault,
                                  ),
                                ),
                              ],
                            ),
                          ),
                          _section('Browsing'),
                          CheckboxListTile(
                            contentPadding: EdgeInsets.zero,
                            dense: true,
                            controlAffinity: ListTileControlAffinity.leading,
                            value: _browse,
                            onChanged: (v) =>
                                setState(() => _browse = v ?? false),
                            title: const Text('Read any site without asking'),
                            subtitle: const Text(
                              'For research agents. Otherwise, once it has '
                              'read a web page or an email, each new site asks '
                              "first, so injected text can't send what it "
                              'knows somewhere new.',
                            ),
                          ),
                          _section('Autonomy ceiling'),
                          AutonomyPicker(
                            value: _autonomy,
                            onChanged: (v) => setState(() => _autonomy = v),
                          ),
                          if (widget.editing case final a?
                              when widget.store.agents.any(
                                (o) => o.id != a.id && !o.archived,
                              )) ...[
                            _section('Can hand work to'),
                            _DelegatesPicker(
                              agent: a,
                              agents: [
                                for (final o in widget.store.agents)
                                  if (o.id != a.id && !o.archived) o,
                              ],
                              chosen: _delegates,
                              onChanged: (v) => setState(() => _delegates = v),
                            ),
                          ],
                          if (widget.editing case final a?
                              when !a.isDefault) ...[
                            _section('Telegram bot'),
                            TelegramBotField(
                              agent: a,
                              agents: widget.store.agents,
                              secrets: _secrets,
                              loadError: _secretsError,
                              value: _bot,
                              onChanged: (v) => setState(() => _bot = v),
                              onSetSecret: () async {
                                await showSecretsDialog(context, widget.store);
                                if (mounted) _loadSecrets();
                              },
                            ),
                          ],
                          const SizedBox(height: 8),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
            const Hairline(),
            if (_error case final e?)
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 8, 20, 0),
                child: Text(
                  e,
                  key: const Key('agent-sheet-error'),
                  style: TextStyle(color: c.error, fontSize: 12.5),
                ),
              ),
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 8, 20, 12),
              child: Row(
                children: [
                  if (editing != null && !_isDefault)
                    TextButton(
                      onPressed: _busy
                          ? null
                          : () async {
                              Navigator.of(context).pop();
                              await widget.store.archiveAgent(editing);
                            },
                      child: Text('Archive', style: TextStyle(color: c.error)),
                    ),
                  if (editing != null && !_isDefault)
                    TextButton(
                      key: const Key('delete-agent'),
                      onPressed: _busy
                          ? null
                          : () async {
                              final nav = Navigator.of(context);
                              await showDeleteAgent(
                                context,
                                widget.store,
                                editing,
                              );
                              // Deleted: there is nothing left to edit.
                              if (mounted &&
                                  widget.store.agentById(editing.id) == null) {
                                nav.pop();
                              }
                            },
                      child: Text('Delete…', style: TextStyle(color: c.error)),
                    ),
                  const Spacer(),
                  TextButton(
                    onPressed: () => Navigator.of(context).pop(),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: ok ? _save : null,
                    child: Text(editing == null ? 'Create' : 'Save'),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  void dispose() {
    _name.dispose();
    _slugCtl.dispose();
    _persona.dispose();
    super.dispose();
  }
}

/// "Can hand work to": the agents this one may delegate to with
/// `agent_spawn {agent}` (agent profiles §6.1).
class _DelegatesPicker extends StatelessWidget {
  const _DelegatesPicker({
    required this.agent,
    required this.agents,
    required this.chosen,
    required this.onChanged,
  });

  final Agent agent;
  final List<Agent> agents;

  /// Edited so far; null = the agent's own setting.
  final List<String>? chosen;
  final ValueChanged<List<String>> onChanged;

  @override
  Widget build(BuildContext context) {
    final on =
        chosen ??
        [
          for (final o in agents)
            if (agent.mayDelegateTo(o)) o.id,
        ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        UnderlineTabs(
          tabs: [
            for (final o in agents)
              UnderlineTab(
                key: ValueKey('delegate-${o.id}'),
                leading: AgentAvatar.of(o, size: 18),
                label: o.name,
                fontSize: 12.5,
                exclusive: false,
                selected: on.contains(o.id),
                onTap: () => onChanged([
                  for (final id in on)
                    if (id != o.id) id,
                  if (!on.contains(o.id)) o.id,
                ]),
              ),
          ],
        ),
        const SizedBox(height: 4),
        Text(
          'It sends them tasks that match their job, and they report back.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    );
  }
}

class _MemoryChoice extends StatelessWidget {
  const _MemoryChoice({
    required this.value,
    required this.title,
    required this.line,
    required this.enabled,
  });

  final String value;
  final String title;
  final String line;
  final bool enabled;

  @override
  Widget build(BuildContext context) => RadioListTile<String>(
    value: value,
    enabled: enabled,
    dense: true,
    contentPadding: EdgeInsets.zero,
    title: Text(title, style: const TextStyle(fontSize: 13)),
    subtitle: Text(line, style: Theme.of(context).textTheme.bodySmall),
  );
}

class _AvatarPicker extends StatelessWidget {
  const _AvatarPicker({
    required this.glyph,
    required this.color,
    required this.onGlyph,
    required this.onColor,
  });

  final String glyph;
  final String color;
  final ValueChanged<String> onGlyph;
  final ValueChanged<String> onColor;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Wrap(
          spacing: 4,
          children: [
            for (final g in AgentAvatar.glyphs)
              InkWell(
                borderRadius: BorderRadius.circular(8),
                onTap: () => onGlyph(g),
                child: Container(
                  width: 30,
                  height: 30,
                  alignment: Alignment.center,
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(8),
                    border: Border.all(
                      color: g == glyph ? c.accent : c.hairline,
                      width: g == glyph ? 1.5 : 1,
                    ),
                  ),
                  child: Text(g, style: const TextStyle(fontSize: 15)),
                ),
              ),
          ],
        ),
        const SizedBox(height: 8),
        Wrap(
          spacing: 6,
          children: [
            for (final name in AgentAvatar.colorNames)
              Tooltip(
                message: name,
                child: InkWell(
                  customBorder: const CircleBorder(),
                  onTap: () => onColor(name),
                  child: Container(
                    width: 22,
                    height: 22,
                    decoration: BoxDecoration(
                      color: AgentAvatar.colorFor(name, name),
                      shape: BoxShape.circle,
                      border: name == color
                          ? Border.all(color: c.text, width: 2)
                          : null,
                    ),
                  ),
                ),
              ),
          ],
        ),
      ],
    );
  }
}

/// Grouped checkboxes for a tool scope (§1.2): built-in tools, connectors,
/// MCP servers (with their status), skills.
class _ToolScopeEditor extends StatelessWidget {
  const _ToolScopeEditor({
    required this.scope,
    required this.builtins,
    required this.connectors,
    required this.mcp,
    required this.onChanged,
  });

  /// Null: everything (the default agent).
  final ToolScope? scope;
  final List<String> builtins;
  final List<String> connectors;
  final List<McpStatus> mcp;
  final ValueChanged<ToolScope?> onChanged;

  ToolScope get _s => scope ?? const ToolScope();

  bool _builtinOn(String t) =>
      _s.builtins.all ||
      (_s.builtins.readOnly && readOnlyTools.contains(t)) ||
      _s.builtins.names.contains(t);

  List<String> _toggle(List<String> on, String name, bool value) => [
    for (final n in on)
      if (n != name) n,
    if (value) name,
  ];

  void _setBuiltin(String t, bool v) {
    final on = [
      for (final b in builtins)
        if (_builtinOn(b)) b,
    ];
    onChanged(
      ToolScope(
        builtins: Pick.names(_toggle(on, t, v)),
        connectors: _s.connectors,
        mcp: _s.mcp,
        skills: _s.skills,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    Widget group(String title, List<Widget> rows, {Widget? trailing}) =>
        Padding(
          padding: const EdgeInsets.only(bottom: 6),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text(
                      title,
                      style: const TextStyle(
                        fontSize: 12.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  ?trailing,
                ],
              ),
              Wrap(spacing: 2, runSpacing: 0, children: rows),
            ],
          ),
        );
    Widget box(
      String label,
      bool value,
      ValueChanged<bool> onChanged, {
      String? note,
    }) => SizedBox(
      width: 240,
      child: CheckboxListTile(
        dense: true,
        visualDensity: VisualDensity.compact,
        contentPadding: EdgeInsets.zero,
        controlAffinity: ListTileControlAffinity.leading,
        value: value,
        onChanged: (v) => onChanged(v ?? false),
        title: Text(label, style: mono.copyWith(fontSize: 12)),
        subtitle: note == null
            ? null
            : Text(note, style: small?.copyWith(color: c.warn)),
      ),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (scope == null)
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: Text(
              'Every tool, connector, MCP server and skill.',
              style: small,
            ),
          ),
        group(
          'Built-in tools',
          [
            for (final t in builtins)
              box(t, _builtinOn(t), (v) => _setBuiltin(t, v)),
          ],
          trailing: TextButton(
            onPressed: () => onChanged(
              ToolScope(
                builtins: const Pick.readOnly(),
                connectors: _s.connectors,
                mcp: _s.mcp,
                skills: _s.skills,
              ),
            ),
            child: const Text('Read-only', style: TextStyle(fontSize: 12)),
          ),
        ),
        if (connectors.isNotEmpty)
          group('Connectors', [
            for (final n in connectors)
              box(
                n,
                _s.connectors.allows(n),
                (v) => onChanged(
                  ToolScope(
                    builtins: _s.builtins,
                    connectors: Pick.names(
                      _toggle(
                        _s.connectors.all ? connectors : _s.connectors.names,
                        n,
                        v,
                      ),
                    ),
                    mcp: _s.mcp,
                    skills: _s.skills,
                  ),
                ),
              ),
          ]),
        if (mcp.isNotEmpty)
          group('MCP servers', [
            for (final m in mcp)
              box(
                m.name,
                _s.mcp.allows(m.name),
                note: switch (m.state) {
                  McpUnavailable() => 'unavailable',
                  _ => null,
                },
                (v) => onChanged(
                  ToolScope(
                    builtins: _s.builtins,
                    connectors: _s.connectors,
                    mcp: Pick.names(
                      _toggle(
                        _s.mcp.all
                            ? [for (final x in mcp) x.name]
                            : _s.mcp.names,
                        m.name,
                        v,
                      ),
                    ),
                    skills: _s.skills,
                  ),
                ),
              ),
          ]),
        group('Skills', [
          box(
            'all skills',
            _s.skills.all,
            (v) => onChanged(
              ToolScope(
                builtins: _s.builtins,
                connectors: _s.connectors,
                mcp: _s.mcp,
                skills: v ? const Pick.all() : const Pick.names([]),
              ),
            ),
          ),
        ]),
        Text(
          'Always included: reading and posting in channels, history, '
          'asking for secrets, and its own memory.',
          style: small,
        ),
      ],
    );
  }
}

/// `chain` with `model` first, its other fallbacks kept: what picking a
/// model does to a chain (T6a's `with_primary`).
List<String> withPrimary(List<String> chain, String model) => [
  model,
  for (final m in chain.skip(1))
    if (m != model) m,
];

/// A model setting of the agent sheet: a dropdown-style field that opens
/// the app's model list (with "Default" first when the setting may be
/// unset). A chain's primary is the pick; its fallbacks stay, shown
/// read-only. A saved model the providers don't list is kept and flagged.
class AgentModelField extends StatelessWidget {
  const AgentModelField({
    super.key,
    required this.store,
    required this.chain,
    required this.onChanged,
    this.allowDefault = true,
    this.defaultLabel = 'Default',
    this.defaultDetail,
    this.label,
  });

  final TernStore store;

  /// A floating label for the field; none = unlabelled.
  final String? label;

  /// Empty: the default.
  final List<String> chain;
  final ValueChanged<List<String>> onChanged;
  final bool allowDefault;
  final String defaultLabel;

  /// What the default resolves to; `default_model` when unset.
  final String? defaultDetail;

  /// Whether the providers' lists, once loaded, include `model`.
  bool? _listed(String model) {
    final lists = store.modelLists;
    if (lists == null) return null;
    final colon = model.indexOf(':');
    if (colon < 0) return false;
    final provider = model.substring(0, colon);
    final id = model.substring(colon + 1);
    return lists.any(
      (l) => l.provider == provider && l.models.any((m) => m.id == id),
    );
  }

  Future<void> _open(BuildContext context) async {
    if (store.modelLists == null && !store.loadingModels) {
      unawaited(store.loadModels());
    }
    final picked = await showDialog<String>(
      context: context,
      builder: (_) => _ModelChooser(
        store: store,
        current: chain.firstOrNull,
        defaultLabel: allowDefault ? defaultLabel : null,
        defaultDetail: defaultDetail,
      ),
    );
    if (picked == null) return;
    onChanged(picked.isEmpty ? const [] : withPrimary(chain, picked));
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final primary = chain.firstOrNull;
    final fallback = defaultDetail ?? store.info?.defaultModel;
    final unknown = primary != null && _listed(primary) == false;
    return ListenableBuilder(
      listenable: store,
      builder: (context, _) => Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(8),
            onTap: () => _open(context),
            child: InputDecorator(
              isEmpty: false,
              decoration: InputDecoration(
                labelText: label,
                isDense: true,
                border: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(8),
                ),
                suffixIcon: Icon(Icons.unfold_more, size: 18, color: c.muted),
              ),
              child: Row(
                children: [
                  if (unknown) ...[
                    Tooltip(
                      message:
                          "Your providers don't list this model (a provider "
                          'was removed, or the value was typed by hand). It '
                          'is kept as it is; pick another to change it.',
                      child: Icon(
                        Icons.warning_amber_rounded,
                        size: 16,
                        color: c.warn,
                      ),
                    ),
                    const SizedBox(width: 6),
                  ],
                  Expanded(
                    child: primary == null
                        ? Text.rich(
                            TextSpan(
                              children: [
                                TextSpan(text: defaultLabel),
                                if (fallback != null)
                                  TextSpan(
                                    text: '  $fallback',
                                    style: mono.copyWith(
                                      fontSize: 12,
                                      color: c.muted,
                                    ),
                                  ),
                              ],
                            ),
                            style: const TextStyle(fontSize: 13),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          )
                        : Text(
                            primary,
                            overflow: TextOverflow.ellipsis,
                            style: mono.copyWith(fontSize: 12.5),
                          ),
                  ),
                ],
              ),
            ),
          ),
          if (unknown)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                'Not in your providers\' model lists.',
                style: TextStyle(fontSize: 12, color: c.warn),
              ),
            ),
          if (chain.isNotEmpty)
            _FallbackSection(store: store, chain: chain, onChanged: onChanged),
        ],
      ),
    );
  }
}

/// Under a model field: the fallbacks in a line, and the visual editor
/// (reorder, add, remove) behind "Edit fallbacks".
class _FallbackSection extends StatefulWidget {
  const _FallbackSection({
    required this.store,
    required this.chain,
    required this.onChanged,
  });

  final TernStore store;
  final List<String> chain;
  final ValueChanged<List<String>> onChanged;

  @override
  State<_FallbackSection> createState() => _FallbackSectionState();
}

class _FallbackSectionState extends State<_FallbackSection> {
  var _open = false;

  @override
  Widget build(BuildContext context) {
    final chain = widget.chain;
    final small = Theme.of(context).textTheme.bodySmall;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (!_open)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              chain.length > 1
                  ? 'Then, if it fails: ${chain.skip(1).join(' → ')}. These '
                        'fallbacks stay when you pick another model.'
                  : 'No fallbacks: if this model fails, so does the message.',
              style: small,
            ),
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const Key('toggle-fallbacks'),
            style: TextButton.styleFrom(
              padding: const EdgeInsets.symmetric(horizontal: 4),
              visualDensity: VisualDensity.compact,
            ),
            onPressed: () => setState(() => _open = !_open),
            child: Text(
              _open
                  ? 'Done with fallbacks'
                  : chain.length > 1
                  ? 'Edit fallbacks'
                  : 'Add fallbacks',
              style: const TextStyle(fontSize: 12.5),
            ),
          ),
        ),
        if (_open)
          Padding(
            padding: const EdgeInsets.only(bottom: 4),
            child: ModelChainEditor(
              store: widget.store,
              chain: chain,
              onChanged: widget.onChanged,
            ),
          ),
      ],
    );
  }
}

/// The app's model list in a dialog, with the default first. Returns the
/// model, `''` for the default, or null when dismissed. Typing a
/// `provider:model` the list doesn't have is the "Other…" way in.
class _ModelChooser extends StatelessWidget {
  const _ModelChooser({
    required this.store,
    required this.current,
    required this.defaultLabel,
    this.defaultDetail,
  });

  final TernStore store;
  final String? current;
  final String? defaultDetail;

  /// Null: no default choice (the default agent's own model).
  final String? defaultLabel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 480),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 14, 16, 12),
          child: ListenableBuilder(
            listenable: store,
            builder: (context, _) => Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (defaultLabel case final label?)
                  ListTile(
                    key: const Key('model-default'),
                    dense: true,
                    contentPadding: const EdgeInsets.symmetric(horizontal: 8),
                    selected: current == null,
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                    leading: Icon(
                      current == null ? Icons.check : Icons.auto_mode,
                      size: 17,
                    ),
                    title: Text(label, style: const TextStyle(fontSize: 13)),
                    subtitle: Text(
                      defaultDetail ??
                          store.info?.defaultModel ??
                          'No default model yet',
                      style: mono.copyWith(fontSize: 11.5, color: c.muted),
                    ),
                    onTap: () => Navigator.of(context).pop(''),
                  ),
                if (store.daemonTooOld)
                  Padding(
                    padding: const EdgeInsets.symmetric(vertical: 6),
                    child: Text(
                      'This tern can\'t list models yet: type one as '
                      'provider:model.',
                      style: TextStyle(fontSize: 12.5, color: c.warn),
                    ),
                  ),
                const SizedBox(height: 6),
                ModelList(
                  lists: store.modelLists,
                  loading: store.loadingModels,
                  current: current,
                  maxHeight: 300,
                  onChoose: (m) => Navigator.of(context).pop(m),
                ),
                const SizedBox(height: 4),
                Text(
                  'Other: type provider:model in the search box.',
                  style: TextStyle(fontSize: 11.5, color: c.muted),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
