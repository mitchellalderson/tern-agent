import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'mcp_sheet.dart';
import 'tools.dart';
import 'widgets.dart';

/// Library → MCP servers (docs/mcp-management-spec.md §8.2): the list, or
/// one server's details, in the centre column.
class McpView extends StatelessWidget {
  const McpView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final open = store.mcpServers
        .where((s) => s.name == store.mcpSelected)
        .firstOrNull;
    final owner = store.health?.isOwner ?? false;
    return Column(
      children: [
        WindowDragArea(
          child: Container(
            height: titleBarInset + 12,
            padding: const EdgeInsets.fromLTRB(12, 0, 10, 0),
            child: Row(
              children: [
                if (open != null)
                  QuietIconButton(
                    icon: Icons.arrow_back,
                    tooltip: 'All MCP servers',
                    onPressed: () => store.openMcp(),
                  )
                else
                  const SizedBox(width: 8),
                const SizedBox(width: 4),
                Expanded(
                  child: Text(
                    open?.name ?? 'MCP servers',
                    style: const TextStyle(
                      fontSize: 15,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
                if (open == null && owner) ...[
                  TextButton.icon(
                    onPressed: () => showMcpPaste(context, store),
                    icon: const Icon(Icons.content_paste, size: 15),
                    label: const Text('Paste config'),
                    style: TextButton.styleFrom(foregroundColor: c.muted),
                  ),
                  const SizedBox(width: 4),
                  FilledButton.icon(
                    onPressed: () => showMcpSheet(context, store),
                    icon: const Icon(Icons.add, size: 16),
                    label: const Text('Add server'),
                  ),
                  const SizedBox(width: 6),
                ],
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
        Expanded(
          child: open != null
              ? McpDetail(store: store, server: open)
              : McpList(store: store),
        ),
      ],
    );
  }
}

class McpList extends StatelessWidget {
  const McpList({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final servers = store.mcpServers;
    if (servers.isEmpty) {
      return Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Text(
            'No MCP servers yet. Add one to give the agent its tools: a '
            'local command, or a remote URL. Most servers document a JSON '
            'snippet you can paste.',
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
      );
    }
    return ListView(
      padding: const EdgeInsets.symmetric(vertical: 12),
      children: [
        for (final s in servers)
          Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 760),
              child: _ServerRow(store: store, server: s),
            ),
          ),
      ],
    );
  }
}

class _ServerRow extends StatelessWidget {
  const _ServerRow({required this.store, required this.server});

  final TernStore store;
  final McpStatus server;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = server;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 1),
      child: InkWell(
        borderRadius: BorderRadius.circular(8),
        onTap: () => store.openMcp(s.name),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 5),
                child: McpStateDot(s.state),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Text(
                          s.name,
                          style: const TextStyle(
                            fontSize: 13.5,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        const SizedBox(width: 8),
                        McpChip(s.config.transport.wire, c.muted),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Text(
                            mcpStateText(s.state),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(fontSize: 12, color: c.muted),
                          ),
                        ),
                      ],
                    ),
                    if (s.state case McpUnavailable(
                      :final error,
                      :final retryAt,
                    ))
                      Padding(
                        padding: const EdgeInsets.only(top: 3),
                        child: Text.rich(
                          TextSpan(
                            children: [
                              if (retryAt != null)
                                WidgetSpan(
                                  alignment: PlaceholderAlignment.baseline,
                                  baseline: TextBaseline.alphabetic,
                                  child: RetryCountdown(
                                    retryAt,
                                    color: c.error,
                                  ),
                                ),
                              TextSpan(text: error),
                            ],
                          ),
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(fontSize: 12, color: c.error),
                        ),
                      ),
                    if (s.missingSecrets.isNotEmpty)
                      Padding(
                        padding: const EdgeInsets.only(top: 3),
                        child: Text(
                          'Missing ${s.missingSecrets.join(', ')}',
                          style: TextStyle(fontSize: 12, color: c.warn),
                        ),
                      ),
                  ],
                ),
              ),
              Switch(
                value: s.config.enabled,
                onChanged: (store.health?.isOwner ?? false)
                    ? (on) => _write(
                        context,
                        store,
                        (c) => c.patchMcp(s.name, enabled: on),
                      )
                    : null,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// One server: its config, tools and stderr.
class McpDetail extends StatefulWidget {
  const McpDetail({super.key, required this.store, required this.server});

  final TernStore store;
  final McpStatus server;

  @override
  State<McpDetail> createState() => _McpDetailState();
}

class _McpDetailState extends State<McpDetail> {
  var _logs = false;
  var _restarting = false;

  /// `GET /v1/mcp/{name}`, for the fresh stderr tail; the list's copy
  /// otherwise.
  McpStatus? _fresh;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  @override
  void didUpdateWidget(McpDetail old) {
    super.didUpdateWidget(old);
    if (old.server != widget.server) _refresh();
  }

  Future<void> _refresh() async {
    try {
      final s = await widget.store.client.mcpServer(widget.server.name);
      if (mounted) setState(() => _fresh = s);
    } on TernException {
      // The list's copy is enough to show.
    }
  }

  McpStatus get _server {
    final f = _fresh;
    return f != null && f.name == widget.server.name ? f : widget.server;
  }

  Future<void> _restart() async {
    setState(() => _restarting = true);
    await _write(context, widget.store, (c) => c.restartMcp(_server.name));
    if (mounted) setState(() => _restarting = false);
  }

  Future<void> _remove() async {
    final s = _server;
    final secrets = s.config.secretNames;
    var removeSecrets = true;
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setLocal) => AlertDialog(
          title: Text('Remove ${s.name}?'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                'Its ${s.tools.length} tools go away from the next run. '
                'Allowlist entries for mcp__${s.name}__* stay.',
              ),
              if (secrets.isNotEmpty)
                CheckboxListTile(
                  value: removeSecrets,
                  contentPadding: EdgeInsets.zero,
                  controlAffinity: ListTileControlAffinity.leading,
                  title: Text(
                    'Also delete its secrets (${secrets.join(', ')}) '
                    'unless something else uses them',
                  ),
                  onChanged: (v) => setLocal(() => removeSecrets = v!),
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
    if (!(ok ?? false) || !mounted) return;
    final store = widget.store;
    final err = await store.mcpWrite(
      (c) => c.removeMcp(
        s.name,
        removeSecrets: removeSecrets && secrets.isNotEmpty,
      ),
    );
    if (err == null) {
      store.openMcp();
    } else if (mounted) {
      _showError(context, err);
    }
  }

  void _toggleTool(McpToolInfo t, bool on) {
    final s = _server;
    final disabled = {...s.config.disabledTools};
    on ? disabled.remove(t.remote) : disabled.add(t.remote);
    _write(
      context,
      widget.store,
      (c) => c.patchMcp(s.name, disabledTools: disabled.toList()..sort()),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = _server;
    final cfg = s.config;
    final owner = widget.store.health?.isOwner ?? false;
    final labels = Theme.of(context).textTheme.labelSmall;
    final summary = <(String, String)>[
      if (cfg.transport == McpTransport.stdio) ...[
        ('Command', [?cfg.command, ...cfg.args].join(' ')),
        if (cfg.cwd case final d?) ('Directory', d),
        if (cfg.env.isNotEmpty)
          (
            'Environment',
            cfg.env.entries.map((e) => '${e.key}=${e.value}').join('  '),
          ),
      ] else ...[
        ('URL', cfg.url ?? ''),
        if (cfg.bearerTokenSecret case final b?) ('Bearer token', b),
        for (final MapEntry(:key, :value) in cfg.headerSecrets.entries)
          ('Header $key', value),
      ],
      if (cfg.envSecrets.isNotEmpty) ('Secrets', cfg.envSecrets.join(', ')),
      if (cfg.requiresApproval) ('Approval', 'Asks before each call'),
      if (cfg.idempotent) ('Retries', 'Safe to retry'),
    ];
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 18, 24, 24),
      children: [
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 712),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    McpStateDot(s.state),
                    const SizedBox(width: 8),
                    McpChip(cfg.transport.wire, c.muted),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        [mcpStateText(s.state), ?s.serverInfo].join(' · '),
                        style: TextStyle(fontSize: 12.5, color: c.muted),
                      ),
                    ),
                    if (owner) ...[
                      TextButton(
                        onPressed: () =>
                            showMcpSheet(context, widget.store, editing: s),
                        child: const Text('Edit'),
                      ),
                      TextButton(
                        onPressed: _restarting || !cfg.enabled
                            ? null
                            : _restart,
                        child: Text(_restarting ? 'Restarting…' : 'Restart'),
                      ),
                      TextButton(
                        onPressed: _remove,
                        style: TextButton.styleFrom(foregroundColor: c.error),
                        child: const Text('Remove'),
                      ),
                    ],
                  ],
                ),
                if (s.state case McpUnavailable(:final error, :final retryAt))
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: Text.rich(
                      TextSpan(
                        children: [
                          TextSpan(text: error),
                          if (retryAt != null)
                            WidgetSpan(
                              alignment: PlaceholderAlignment.baseline,
                              baseline: TextBaseline.alphabetic,
                              child: RetryCountdown(
                                retryAt,
                                prefix: ' · ',
                                color: c.error,
                              ),
                            ),
                        ],
                      ),
                      style: TextStyle(fontSize: 12.5, color: c.error),
                    ),
                  ),
                if (s.missingSecrets.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: Text(
                      'Missing ${s.missingSecrets.join(', ')}: set '
                      '${s.missingSecrets.length == 1 ? 'it' : 'them'} with '
                      'Edit.',
                      style: TextStyle(fontSize: 12.5, color: c.warn),
                    ),
                  ),
                const SizedBox(height: 18),
                Text('Config', style: labels),
                const SizedBox(height: 6),
                for (final (k, v) in summary)
                  Padding(
                    padding: const EdgeInsets.symmetric(vertical: 3),
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        SizedBox(
                          width: 120,
                          child: Text(
                            k,
                            style: TextStyle(fontSize: 12.5, color: c.muted),
                          ),
                        ),
                        Expanded(
                          child: SelectableText(
                            v,
                            style: mono.copyWith(fontSize: 12.5),
                          ),
                        ),
                      ],
                    ),
                  ),
                const SizedBox(height: 20),
                Text('Tools · ${s.tools.length}', style: labels),
                const SizedBox(height: 4),
                if (s.tools.isEmpty)
                  Text(
                    s.state is McpRunning
                        ? 'This server offers no tools.'
                        : 'Tools show here once it has connected.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                for (final t in s.tools)
                  InkWell(
                    borderRadius: BorderRadius.circular(6),
                    // Hidden tools aren't in the agent's list to look up.
                    onTap: t.disabled
                        ? null
                        : () => showDialog<void>(
                            context: context,
                            builder: (_) => ToolDialog(
                              store: widget.store,
                              name: t.exposed,
                            ),
                          ),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 2),
                      child: Row(
                        children: [
                          Expanded(
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text(
                                  t.remote,
                                  style: mono.copyWith(
                                    fontSize: 12.5,
                                    color: t.disabled ? c.muted : c.text,
                                  ),
                                ),
                                if (t.description.isNotEmpty)
                                  Text(
                                    t.description.split('\n').first,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: TextStyle(
                                      fontSize: 11.5,
                                      color: c.muted,
                                    ),
                                  ),
                              ],
                            ),
                          ),
                          Switch(
                            value: !t.disabled,
                            onChanged: owner
                                ? (on) => _toggleTool(t, on)
                                : null,
                          ),
                        ],
                      ),
                    ),
                  ),
                const SizedBox(height: 16),
                InkWell(
                  borderRadius: BorderRadius.circular(6),
                  onTap: () => setState(() => _logs = !_logs),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 4),
                    child: Row(
                      children: [
                        Icon(
                          _logs ? Icons.expand_more : Icons.chevron_right,
                          size: 16,
                          color: c.muted,
                        ),
                        const SizedBox(width: 4),
                        Text(
                          'Logs · ${s.stderrTail.length} lines',
                          style: labels,
                        ),
                        const Spacer(),
                        if (_logs && s.stderrTail.isNotEmpty)
                          CopyButton(
                            text: () => s.stderrTail.join('\n'),
                            tooltip: 'Copy logs',
                          ),
                      ],
                    ),
                  ),
                ),
                if (_logs)
                  McpLogs(
                    s.stderrTail,
                    empty: cfg.transport == McpTransport.stdio
                        ? 'Nothing on stderr yet.'
                        : 'HTTP servers have no stderr to show.',
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

/// Run an MCP write, showing the daemon's reason if it fails.
Future<void> _write(
  BuildContext context,
  TernStore store,
  Future<Object?> Function(TernClient c) write,
) async {
  final err = await store.mcpWrite(write);
  if (err != null && context.mounted) _showError(context, err);
}

void _showError(BuildContext context, String message) =>
    ScaffoldMessenger.maybeOf(context)?.showSnackBar(
      SnackBar(content: Text(message), behavior: SnackBarBehavior.floating),
    );

/// A server's state in a few words.
String mcpStateText(McpState s) => switch (s) {
  McpDisabled() => 'disabled',
  McpNotStarted() => 'not started',
  McpConnecting() => 'connecting…',
  McpRunning(:final tools) =>
    'running · $tools ${tools == 1 ? 'tool' : 'tools'}',
  McpUnavailable(:final failures) =>
    'unavailable${failures > 1 ? ' · $failures failures' : ''}',
};

class McpStateDot extends StatelessWidget {
  const McpStateDot(this.state, {super.key});

  final McpState state;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final (color, hollow) = switch (state) {
      McpRunning() => (c.ok, false),
      McpUnavailable() => (c.error, false),
      McpConnecting() => (c.warn, false),
      McpNotStarted() => (c.muted, false),
      McpDisabled() => (c.muted, true),
    };
    return Tooltip(
      message: mcpStateText(state),
      child: Container(
        width: 8,
        height: 8,
        decoration: BoxDecoration(
          color: hollow ? null : color,
          border: hollow ? Border.all(color: color, width: 1.2) : null,
          shape: BoxShape.circle,
        ),
      ),
    );
  }
}

class McpChip extends StatelessWidget {
  const McpChip(this.label, this.color, {super.key});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) =>
      Text(label, style: mono.copyWith(fontSize: 11, color: color));
}

/// "retry in 12s", ticking down.
class RetryCountdown extends StatefulWidget {
  const RetryCountdown(
    this.at, {
    super.key,
    this.prefix = '',
    this.color,
    this.now,
  });

  final DateTime at;
  final String prefix;

  /// A widget span doesn't inherit its paragraph's style, so pass it.
  final Color? color;

  /// For tests.
  final DateTime Function()? now;

  @override
  State<RetryCountdown> createState() => _RetryCountdownState();
}

class _RetryCountdownState extends State<RetryCountdown> {
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (_left <= Duration.zero) _tick?.cancel();
      setState(() {});
    });
  }

  @override
  void dispose() {
    _tick?.cancel();
    super.dispose();
  }

  Duration get _left =>
      widget.at.difference((widget.now ?? DateTime.now).call());

  @override
  Widget build(BuildContext context) {
    final s = _left.inSeconds;
    final text = s <= 0
        ? 'retrying'
        : s < 120
        ? 'retry in ${s}s'
        : 'retry in ${(s / 60).ceil()}m';
    return Text(
      widget.prefix.isEmpty ? '$text · ' : '${widget.prefix}$text',
      style: TextStyle(
        fontSize: 12,
        color: widget.color,
        fontWeight: FontWeight.w600,
      ),
    );
  }
}

/// A stderr tail in mono.
class McpLogs extends StatelessWidget {
  const McpLogs(this.lines, {super.key, this.empty = 'No output.'});

  final List<String> lines;
  final String empty;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      width: double.infinity,
      constraints: const BoxConstraints(maxHeight: 260),
      margin: const EdgeInsets.only(top: 4),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: lines.isEmpty
          ? Text(empty, style: TextStyle(fontSize: 12, color: c.muted))
          : SingleChildScrollView(
              reverse: true,
              child: SelectableText(
                lines.join('\n'),
                style: mono.copyWith(fontSize: 11.5, color: c.text),
              ),
            ),
    );
  }
}
