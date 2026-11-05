import 'dart:io';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/connection.dart';
import '../state/daemon.dart';
import '../state/store.dart';
import '../state/version.dart';
import '../theme.dart';
import 'config_editor.dart';
import 'providers.dart';
import 'secrets.dart';
import 'security.dart';
import 'servers.dart';
import 'settings.dart';

export '../state/version.dart' show appVersion;

/// The platform Settings window's sections. Agent settings (an agent's model,
/// tools, autonomy, channels) are not here: they stay in the agent sheet.
enum PrefSection {
  general(
    'General',
    Icons.tune,
    'launch login startup close window quit tray menu bar workspace quiet '
        'hours initiative digest memory skills where tern runs',
  ),
  models(
    'Models & Providers',
    Icons.auto_awesome_outlined,
    'provider api key default model fallback chain sub-agent model',
  ),
  secrets(
    'Secrets',
    Icons.key_outlined,
    'api key token password secrets.toml environment',
  ),
  permissions(
    'Permissions & Sandbox',
    Icons.shield_outlined,
    'approvals autonomy sandbox network shell security audit taint scripts '
        'bundles outbox',
  ),
  limits(
    'Limits & Budgets',
    Icons.speed,
    'budget spend cap usd tokens timeout iterations sub-agents result size',
  ),
  servers(
    'Servers',
    Icons.dns_outlined,
    'saved servers remote listen api webhooks devices tls address',
  ),
  advanced(
    'Advanced',
    Icons.settings_suggest_outlined,
    'config.toml tuning raw file reveal',
  ),
  about(
    'About',
    Icons.info_outline,
    'version data location folder logs telemetry updates config path',
  );

  const PrefSection(this.title, this.icon, this.keywords);

  final String title;
  final IconData icon;

  /// What searching finds in a section that has no settings rows of its own.
  final String keywords;

  /// The daemon's `group` for the settings this section shows.
  String? get group => switch (this) {
    general => 'general',
    models => 'models',
    permissions => 'permissions',
    limits => 'limits',
    servers => 'servers',
    _ => null,
  };
}

const _groupTitles = {
  'general': 'General',
  'models': 'Models',
  'permissions': 'Permissions & sandbox',
  'limits': 'Limits & budgets',
  'servers': 'Servers & devices',
  'advanced': 'Other',
};

const _intro = {
  PrefSection.general:
      'How tern behaves day to day: where it runs, when it speaks up, what '
      'it remembers.',
  PrefSection.models:
      'The providers tern can use, their API keys, and the model new '
      'conversations start with. Changes apply to the next message.',
  PrefSection.secrets:
      'API keys and tokens for providers, connectors and MCP servers.',
  PrefSection.permissions:
      'What tern may do without asking, and how its shell commands are '
      'confined. These apply to every agent that does not set its own.',
  PrefSection.limits:
      'Caps on how long and how much tern works before it stops or asks.',
  PrefSection.servers:
      'Where tern runs, and how other devices and webhooks reach it.',
  PrefSection.advanced:
      'Tuning knobs most people never touch, and the raw file. Each change '
      'is checked and saved right away, keeping config.toml\'s comments.',
  PrefSection.about:
      'This app, the tern running behind it, and where things '
      'are kept.',
};

/// Opens the Settings window from anywhere under [Home], on [section]; the
/// old Models, Secrets and Configuration entry points all come here. Without
/// the scope (a widget shown on its own) the matching dialog opens instead.
Future<void> openPreferences(
  BuildContext context,
  TernStore store, {
  PrefSection section = PrefSection.general,
}) async {
  final scope = context.getInheritedWidgetOfExactType<PreferencesScope>();
  if (scope != null) return scope.open(section);
  switch (section) {
    case PrefSection.models:
      return showProvidersDialog(context, store);
    case PrefSection.secrets:
      return showSecretsDialog(context, store);
    default:
      return;
  }
}

/// Who can open Settings: [Home] provides it, knowing the app's control,
/// preferences, connections and how to restart tern.
class PreferencesScope extends InheritedWidget {
  const PreferencesScope({super.key, required this.open, required super.child});

  final Future<void> Function(PrefSection section) open;

  @override
  bool updateShouldNotify(PreferencesScope old) => false;
}

Future<void> showPreferences(
  BuildContext context, {
  required TernControl control,
  required AppPrefs prefs,
  TernStore? store,
  Connections? connections,
  Future<void> Function()? onRestart,
  PrefSection section = PrefSection.general,
}) => showDialog<void>(
  context: context,
  builder: (_) => PreferencesDialog(
    control: control,
    prefs: prefs,
    store: store,
    connections: connections,
    onRestart: onRestart,
    section: section,
  ),
);

/// One window for everything that is a platform setting: a sidebar of
/// sections, a search over all of them, and each section's content. It embeds
/// the providers, secrets and servers editors rather than copying them, and
/// shows every `config.toml` setting the daemon describes in the section it
/// belongs to.
class PreferencesDialog extends StatefulWidget {
  const PreferencesDialog({
    super.key,
    required this.control,
    required this.prefs,
    this.store,
    this.connections,
    this.onRestart,
    this.section = PrefSection.general,
    this.reveal = revealFile,
  });

  final TernControl control;
  final AppPrefs prefs;
  final TernStore? store;
  final Connections? connections;

  /// Restarts tern; without it the restart hint only explains.
  final Future<void> Function()? onRestart;
  final PrefSection section;
  final Future<void> Function(String path) reveal;

  @override
  State<PreferencesDialog> createState() => _PreferencesDialogState();
}

class _PreferencesDialogState extends State<PreferencesDialog> {
  late var _section = widget.section;
  final _search = TextEditingController();
  ConfigView? _view;
  String? _error;

  /// Per setting: being saved, or why the last save failed.
  final _busy = <String>{};
  final _errors = <String, String>{};
  var _restarting = false;

  TernClient? get _client => widget.store?.client;

  bool get _local =>
      widget.connections?.isRemote == false ||
      (widget.connections == null &&
          widget.store?.client.endpoint is! RemoteEndpoint);

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final client = _client;
    if (client == null) return;
    try {
      final v = await client.configSettings();
      if (!mounted) return;
      setState(() {
        _view = v;
        _error = null;
      });
    } on TernException catch (e) {
      if (!mounted) return;
      setState(
        () => _error = e.status == 403
            ? 'Only a device paired as owner can see or change the '
                  'configuration.'
            : e.message,
      );
    }
  }

  void _replace(ConfigSetting s) {
    final v = _view;
    if (v == null) return;
    _view = ConfigView(
      path: v.path,
      error: v.error,
      sections: [
        for (final sec in v.sections)
          ConfigSection(
            id: sec.id,
            title: sec.title,
            description: sec.description,
            settings: [for (final x in sec.settings) x.key == s.key ? s : x],
          ),
      ],
    );
  }

  Future<void> _write(
    String key,
    Future<ConfigSetting> Function(TernClient c) op,
  ) async {
    final client = _client;
    if (client == null) return;
    setState(() {
      _busy.add(key);
      _errors.remove(key);
    });
    try {
      final s = await op(client);
      if (mounted) setState(() => _replace(s));
    } on TernException catch (e) {
      if (mounted) setState(() => _errors[key] = e.message);
    } finally {
      if (mounted) setState(() => _busy.remove(key));
    }
  }

  Future<void> _restart() async {
    final r = widget.onRestart;
    if (r == null) return;
    setState(() => _restarting = true);
    try {
      await r();
    } finally {
      if (mounted) Navigator.pop(context);
    }
  }

  void _go(PrefSection s) => setState(() {
    _section = s;
    _search.clear();
  });

  bool _matches(ConfigSetting s, String q) =>
      s.key.toLowerCase().contains(q) ||
      settingLabel(s).toLowerCase().contains(q) ||
      s.description.toLowerCase().contains(q) ||
      (s.security?.toLowerCase().contains(q) ?? false);

  Widget _row(ConfigSetting s) => SettingRow(
    key: ValueKey(s.key),
    setting: s,
    busy: _busy.contains(s.key),
    error: _errors[s.key],
    onChanged: (v) => _write(s.key, (c) => c.setSetting(s.key, v)),
    onReset: () => _write(s.key, (c) => c.resetSetting(s.key)),
  );

  /// The settings rows of one group, or why there are none to show.
  List<Widget> _rows(
    String group, {
    bool advanced = false,
    Set<String> except = const {},
  }) {
    final v = _view;
    if (v == null) return [_viewStatus(context)];
    final list = [
      for (final s in v.inGroup(group, advanced: advanced))
        if (!except.contains(s.key)) s,
    ];
    return [for (final s in list) _row(s)];
  }

  Widget _viewStatus(BuildContext context) {
    final c = context.colors;
    final v = _view;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 16),
      child: Center(
        child: switch ((_error, v?.error)) {
          (final e?, _) => Text(e, style: TextStyle(color: c.error)),
          (_, final e?) => Text(
            "config.toml doesn't load, so nothing can be changed here until "
            'it is fixed:\n\n$e',
            style: TextStyle(color: c.error),
          ),
          _ when widget.store == null => Text(
            'Not connected to tern.',
            style: TextStyle(color: c.muted),
          ),
          _ => const CircularProgressIndicator(strokeWidth: 2),
        },
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final pending = _view?.settings.where((s) => s.restartPending).length ?? 0;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 960, maxHeight: 720),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 18, 20, 14),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const Expanded(
                    child: Text(
                      'Settings',
                      style: TextStyle(
                        fontSize: 17,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Done'),
                  ),
                ],
              ),
              if (pending > 0) ...[
                const SizedBox(height: 6),
                RestartBanner(
                  count: pending,
                  restarting: _restarting,
                  onRestart: widget.onRestart == null ? null : _restart,
                ),
              ],
              const SizedBox(height: 10),
              Flexible(
                child: SizedBox(
                  height: 560,
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      SizedBox(width: 196, child: _sidebar(context)),
                      VerticalDivider(width: 24, color: c.hairline),
                      Expanded(child: _content(context)),
                    ],
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _sidebar(BuildContext context) {
    final c = context.colors;
    final searching = _search.text.trim().isNotEmpty;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        TextField(
          key: const Key('settings-search'),
          controller: _search,
          onChanged: (_) => setState(() {}),
          style: const TextStyle(fontSize: 13),
          decoration: settingsBox(
            context,
            hint: 'Search settings',
            icon: Icon(Icons.search, size: 16, color: c.muted),
          ),
        ),
        const SizedBox(height: 8),
        Expanded(
          child: ListView(
            children: [
              for (final s in PrefSection.values)
                _NavTile(
                  key: Key('settings-nav-${s.name}'),
                  section: s,
                  selected: !searching && s == _section,
                  changed: switch (s.group) {
                    final g? =>
                      _view?.settings
                              .where((x) => x.group == g && x.changed)
                              .length ??
                          0,
                    _ => 0,
                  },
                  risky:
                      s.group != null &&
                      (_view?.settings.any(
                            (x) => x.group == s.group && x.risky,
                          ) ??
                          false),
                  onTap: () => _go(s),
                ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _content(BuildContext context) {
    final q = _search.text.trim().toLowerCase();
    if (q.isNotEmpty) return _results(context, q);
    final c = context.colors;
    return SingleChildScrollView(
      key: ValueKey(_section),
      padding: const EdgeInsets.only(right: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(
            _section.title,
            style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
          ),
          const SizedBox(height: 3),
          Text(
            _intro[_section]!,
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
          const SizedBox(height: 12),
          ..._pane(context),
        ],
      ),
    );
  }

  List<Widget> _pane(BuildContext context) {
    final store = widget.store;
    final c = context.colors;
    Widget heading(String t) => Padding(
      padding: const EdgeInsets.only(top: 18, bottom: 2),
      child: Text(
        t,
        style: TextStyle(
          fontSize: 12,
          fontWeight: FontWeight.w600,
          color: c.muted,
        ),
      ),
    );
    Widget needStore() => Padding(
      padding: const EdgeInsets.symmetric(vertical: 16),
      child: Text('Not connected to tern.', style: TextStyle(color: c.muted)),
    );
    switch (_section) {
      case PrefSection.general:
        return [
          SettingsDialog(
            control: widget.control,
            prefs: widget.prefs,
            store: store,
            connections: widget.connections,
            embedded: true,
            onOpenServers: () => _go(PrefSection.servers),
          ),
          heading('Behavior'),
          ..._rows('general'),
        ];
      case PrefSection.models:
        return [
          if (store == null)
            needStore()
          else
            ProvidersDialog(
              store: store,
              embedded: true,
              onOpenSecrets: () => _go(PrefSection.secrets),
            ),
          heading('Sub-agents'),
          ..._rows('models', except: const {'default_model'}),
        ];
      case PrefSection.secrets:
        return [
          if (store == null)
            needStore()
          else
            SecretsDialog(client: store.client, embedded: true),
        ];
      case PrefSection.permissions:
        return [
          ..._rows('permissions'),
          if (store != null && store.hasSecurityAudit) ...[
            heading('Security'),
            SecurityAuditRow(store: store),
          ],
        ];
      case PrefSection.limits:
        return _rows('limits');
      case PrefSection.servers:
        return [
          if (widget.connections case final conns?)
            ServersDialog(connections: conns, embedded: true)
          else
            Text(
              'This app is not set up to switch servers.',
              style: TextStyle(color: c.muted),
            ),
          heading('Listeners & webhooks'),
          ..._rows('servers'),
        ];
      case PrefSection.advanced:
        return _advanced(context, heading);
      case PrefSection.about:
        return _about(context, heading);
    }
  }

  List<Widget> _advanced(
    BuildContext context,
    Widget Function(String) heading,
  ) {
    final v = _view;
    return [
      if (v != null && _local)
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: () => widget.reveal(v.path),
            icon: const Icon(Icons.description_outlined, size: 16),
            label: const Text('Open config.toml'),
          ),
        ),
      if (v == null) _viewStatus(context),
      if (v != null)
        for (final g in _groupTitles.keys)
          if (v.inGroup(g, advanced: true).isNotEmpty) ...[
            heading(_groupTitles[g]!),
            ..._rows(g, advanced: true),
          ],
    ];
  }

  List<Widget> _about(BuildContext context, Widget Function(String) heading) {
    final c = context.colors;
    final health = widget.store?.health;
    final path = _view?.path;
    final dir = path == null ? null : File(path).parent.path;
    Widget line(String label, String? value, {Widget? trailing}) => Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 150,
            child: Text(label, style: TextStyle(fontSize: 13, color: c.muted)),
          ),
          Expanded(
            child: SelectableText(
              value ?? '…',
              style: mono.copyWith(fontSize: 12.5),
            ),
          ),
          ?trailing,
        ],
      ),
    );
    return [
      line('App', 'tern desktop ${appVersionLabel()}'),
      line('Core', health == null ? 'Not connected' : 'tern ${health.version}'),
      if (health != null) line('Core API', '${health.api}'),
      if (_local) line('Core program', widget.control.bin ?? 'not found'),
      if (widget.store?.coreSkew case final skew?)
        Padding(
          padding: const EdgeInsets.only(top: 4, bottom: 4),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  skew,
                  key: const Key('about-skew'),
                  style: TextStyle(fontSize: 12.5, color: c.warn),
                ),
              ),
              if (widget.onRestart != null)
                TextButton(
                  onPressed: _restarting ? null : _restart,
                  child: const Text('Restart tern'),
                ),
            ],
          ),
        ),
      heading('Where things are kept'),
      line(
        'Data folder',
        dir,
        trailing: dir != null && _local
            ? TextButton(
                onPressed: () => widget.reveal(path!),
                child: const Text('Reveal'),
              )
            : null,
      ),
      line('Config file', path),
      line('Logs', dir == null ? null : '$dir/logs'),
      if (_local) line('This app\'s settings', AppPrefs.dir()),
      if (!_local)
        Padding(
          padding: const EdgeInsets.only(top: 6),
          child: Text(
            'These are on the server, not on this computer.',
            style: TextStyle(fontSize: 12, color: c.muted),
          ),
        ),
      if (_local) ...[
        heading('Back up and uninstall'),
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Text(
            'Everything tern keeps is in the data folder above, API keys '
            'included. To back it up, stop tern, then copy the folder. '
            'Dragging this app to the Trash does not stop tern or remove '
            'its login items: run `tern uninstall` first (it keeps your '
            'data; add --purge to delete it), or use tern › Quit tern '
            'completely.',
            key: const Key('about-uninstall'),
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
        ),
        SelectableText(
          'tern stop\ncp -R "${dir ?? '~/.tern'}" ~/tern-backup\ntern uninstall',
          style: mono.copyWith(fontSize: 12),
        ),
      ],
      heading('Privacy and updates'),
      Padding(
        padding: const EdgeInsets.symmetric(vertical: 6),
        child: Text(
          'tern sends no telemetry and does not check for updates by itself. '
          'To update, install the new version. TERN_HOME moves the data '
          'folder and TERN_LOG sets the log level; both are read when tern '
          'starts.',
          style: TextStyle(fontSize: 12.5, color: c.muted),
        ),
      ),
    ];
  }

  /// Search: settings across every section, and sections whose own editors
  /// match (Secrets, Providers, Servers and so on).
  Widget _results(BuildContext context, String q) {
    final c = context.colors;
    final v = _view;
    final settings = [
      if (v != null)
        for (final s in v.settings)
          if (_matches(s, q)) s,
    ];
    final sections = [
      for (final s in PrefSection.values)
        if (s.title.toLowerCase().contains(q) || s.keywords.contains(q)) s,
    ];
    if (settings.isEmpty && sections.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Text(
          v == null && _error == null
              ? 'Searching…'
              : 'No setting matches “${_search.text.trim()}”.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      );
    }
    return SingleChildScrollView(
      padding: const EdgeInsets.only(right: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final s in sections)
            ListTile(
              key: Key('settings-result-${s.name}'),
              dense: true,
              contentPadding: EdgeInsets.zero,
              leading: Icon(s.icon, size: 18, color: c.muted),
              title: Text(s.title),
              subtitle: Text(
                _intro[s]!,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11.5, color: c.muted),
              ),
              onTap: () => _go(s),
            ),
          for (final s in settings) ...[
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                '${_groupTitles[s.group] ?? s.group}'
                '${s.advanced ? ' · advanced' : ''}',
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: FontWeight.w600,
                  color: c.muted,
                ),
              ),
            ),
            _row(s),
          ],
        ],
      ),
    );
  }
}

class _NavTile extends StatelessWidget {
  const _NavTile({
    super.key,
    required this.section,
    required this.selected,
    required this.changed,
    required this.risky,
    required this.onTap,
  });

  final PrefSection section;
  final bool selected;
  final int changed;
  final bool risky;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.only(bottom: 1),
      child: Material(
        color: selected ? c.selection : Colors.transparent,
        borderRadius: BorderRadius.circular(7),
        child: InkWell(
          borderRadius: BorderRadius.circular(7),
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
            child: Row(
              children: [
                Icon(section.icon, size: 16, color: selected ? c.ink : c.muted),
                const SizedBox(width: 9),
                Expanded(
                  child: Text(
                    section.title,
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                    ),
                  ),
                ),
                if (risky) Icon(Icons.shield_outlined, size: 13, color: c.warn),
                if (changed > 0)
                  Padding(
                    padding: const EdgeInsets.only(left: 4),
                    child: Text(
                      '$changed',
                      style: TextStyle(fontSize: 11, color: c.link),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
