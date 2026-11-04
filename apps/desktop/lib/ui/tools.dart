import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'mcp.dart';
import 'widgets.dart';

/// The side panel's tool list: filter by name or description, click one for
/// its spec. `mcp__*` tools sit under their server, which links to its
/// page.
class ToolsSection extends StatefulWidget {
  const ToolsSection({super.key, required this.store});

  final TernStore store;

  @override
  State<ToolsSection> createState() => _ToolsSectionState();
}

class _ToolsSectionState extends State<ToolsSection> {
  final _query = TextEditingController();

  @override
  void dispose() {
    _query.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final tools = widget.store.info?.tools ?? const [];
    final q = _query.text.trim().toLowerCase();
    final shown = q.isEmpty
        ? tools
        : tools
              .where(
                (t) =>
                    t.name.toLowerCase().contains(q) ||
                    t.description.toLowerCase().contains(q),
              )
              .toList();
    final groups = _groups(shown);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          'TOOLS · ${tools.length}',
          style: Theme.of(context).textTheme.labelSmall,
        ),
        const SizedBox(height: 8),
        SizedBox(
          height: 30,
          child: TextField(
            controller: _query,
            onChanged: (_) => setState(() {}),
            style: const TextStyle(fontSize: 12.5),
            decoration: InputDecoration(
              hintText: 'Search tools',
              hintStyle: TextStyle(color: c.muted),
              prefixIcon: Icon(Icons.search, size: 15, color: c.muted),
              prefixIconConstraints: const BoxConstraints(minWidth: 30),
              isDense: true,
              filled: true,
              fillColor: c.background,
              contentPadding: const EdgeInsets.symmetric(vertical: 8),
              border: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
                borderSide: BorderSide(color: c.hairline),
              ),
              enabledBorder: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
                borderSide: BorderSide(color: c.hairline),
              ),
            ),
          ),
        ),
        const SizedBox(height: 6),
        if (shown.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 6),
            child: Text(
              'No tools match',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
        for (final t in shown)
          if (!t.name.startsWith('mcp__')) _tool(context, t.name, t),
        for (final MapEntry(key: server, value: list) in groups.entries)
          if (q.isEmpty || list.isNotEmpty) ...[
            _serverRow(context, server, list.length),
            for (final (short, t) in list)
              Padding(
                padding: const EdgeInsets.only(left: 14),
                child: _tool(context, short, t),
              ),
          ],
      ],
    );
  }

  /// Tools by MCP server, in the daemon's server order. Every enabled
  /// server gets a group, so one that's down still shows (and links).
  Map<String, List<(String, ({String name, String description}))>> _groups(
    Iterable<({String name, String description})> tools,
  ) {
    final servers = widget.store.mcpServers;
    final out = {
      for (final s in servers)
        if (s.config.enabled)
          s.name: <(String, ({String name, String description}))>[],
    };
    for (final t in tools) {
      if (!t.name.startsWith('mcp__')) continue;
      final rest = t.name.substring(5);
      final known = servers
          .where((s) => rest.startsWith('${s.name}__'))
          .firstOrNull
          ?.name;
      final cut = rest.indexOf('__');
      final server = known ?? (cut > 0 ? rest.substring(0, cut) : 'mcp');
      final short = rest.startsWith('${server}__')
          ? rest.substring(server.length + 2)
          : rest;
      (out[server] ??= []).add((short, t));
    }
    return out;
  }

  Widget _serverRow(BuildContext context, String name, int count) {
    final c = context.colors;
    final s = widget.store.mcpServers.where((s) => s.name == name).firstOrNull;
    return InkWell(
      borderRadius: BorderRadius.circular(6),
      onTap: s == null ? null : () => widget.store.openMcp(name),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(2, 8, 2, 3),
        child: Row(
          children: [
            if (s != null) ...[McpStateDot(s.state), const SizedBox(width: 6)],
            Text(
              name,
              style: mono.copyWith(fontSize: 12, fontWeight: FontWeight.w600),
            ),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                s == null || s.state is McpRunning
                    ? '$count'
                    : mcpStateText(s.state),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 11.5,
                  color: s?.state is McpUnavailable ? c.error : c.muted,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _tool(
    BuildContext context,
    String label,
    ({String name, String description}) t,
  ) => InkWell(
    borderRadius: BorderRadius.circular(6),
    onTap: () => showDialog<void>(
      context: context,
      builder: (_) => ToolDialog(store: widget.store, name: t.name),
    ),
    child: Padding(
      padding: const EdgeInsets.symmetric(vertical: 5, horizontal: 2),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(label, style: mono.copyWith(fontSize: 12)),
          if (t.description.isNotEmpty)
            Text(
              t.description.split('\n').first,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 11.5, color: context.colors.muted),
            ),
        ],
      ),
    ),
  );
}

/// One tool's spec, as the model sees it.
class ToolDialog extends StatefulWidget {
  const ToolDialog({super.key, required this.store, required this.name});

  final TernStore store;
  final String name;

  @override
  State<ToolDialog> createState() => _ToolDialogState();
}

class _ToolDialogState extends State<ToolDialog> {
  ToolSpec? _spec;
  String? _error;
  var _raw = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final specs = await widget.store.client.tools();
      final spec = specs.where((s) => s.name == widget.name).firstOrNull;
      if (!mounted) return;
      setState(() {
        _spec = spec;
        if (spec == null) _error = 'This tool is no longer available.';
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final spec = _spec;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      clipBehavior: Clip.antiAlias,
      child: SizedBox(
        width: 620,
        height: 520,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 14, 8, 10),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      widget.name,
                      style: mono.copyWith(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (spec != null)
                    TextButton(
                      onPressed: () => setState(() => _raw = !_raw),
                      style: TextButton.styleFrom(foregroundColor: c.muted),
                      child: Text(_raw ? 'Formatted' : 'JSON schema'),
                    ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: () => Navigator.pop(context),
                  ),
                ],
              ),
            ),
            const Hairline(),
            Expanded(
              child: switch ((spec, _error)) {
                (_, final String e) => Center(
                  child: Text(e, style: TextStyle(color: c.error)),
                ),
                (null, _) => const Center(
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
                (final ToolSpec s, _) => SingleChildScrollView(
                  padding: const EdgeInsets.fromLTRB(20, 16, 20, 20),
                  child: _raw ? _Schema(s) : _Formatted(s),
                ),
              },
            ),
          ],
        ),
      ),
    );
  }
}

const _effectHelp = {
  'read': 'Never asks.',
  'write': 'Asks unless you chose “Always allow”.',
  'destructive': 'Asks every time.',
};

class _Formatted extends StatelessWidget {
  const _Formatted(this.spec);

  final ToolSpec spec;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final params = spec.params;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('DESCRIPTION', style: labels),
        const SizedBox(height: 6),
        SelectableText(
          spec.description.isEmpty ? '(none)' : spec.description,
          style: const TextStyle(fontSize: 13, height: 1.45),
        ),
        const SizedBox(height: 20),
        Text('EFFECT', style: labels),
        const SizedBox(height: 6),
        Text(
          [
            '${spec.effect} · ${_effectHelp[spec.effect] ?? ''}',
            if (spec.untrusted) 'Returns third-party text.',
          ].join('  '),
          style: const TextStyle(fontSize: 13, height: 1.45),
        ),
        const SizedBox(height: 20),
        Text('ARGUMENTS', style: labels),
        const SizedBox(height: 6),
        if (params.isEmpty)
          Text('None', style: Theme.of(context).textTheme.bodySmall),
        for (final p in params)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 6),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text.rich(
                  TextSpan(
                    children: [
                      TextSpan(
                        text: p.name,
                        style: mono.copyWith(
                          fontSize: 12.5,
                          fontWeight: FontWeight.w600,
                          color: c.text,
                        ),
                      ),
                      TextSpan(
                        text: '  ${p.type}',
                        style: mono.copyWith(fontSize: 12, color: c.muted),
                      ),
                      if (p.required)
                        TextSpan(
                          text: '  required',
                          style: TextStyle(fontSize: 11.5, color: c.warn),
                        ),
                    ],
                  ),
                ),
                if (p.description.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: 2),
                    child: SelectableText(
                      p.description,
                      style: TextStyle(fontSize: 12.5, color: c.muted),
                    ),
                  ),
              ],
            ),
          ),
      ],
    );
  }
}

class _Schema extends StatelessWidget {
  const _Schema(this.spec);

  final ToolSpec spec;

  @override
  Widget build(BuildContext context) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      color: context.colors.card,
      border: Border.all(color: context.colors.hairline),
      borderRadius: BorderRadius.circular(8),
    ),
    child: SelectableText(
      const JsonEncoder.withIndent('  ').convert(spec.inputSchema),
      style: mono.copyWith(fontSize: 12, color: context.colors.text),
    ),
  );
}
