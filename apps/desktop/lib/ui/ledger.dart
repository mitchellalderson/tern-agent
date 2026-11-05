import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'underline_tabs.dart';
import 'widgets.dart';

/// The header every Library page shares: a title, optional actions, and
/// the activity panel's toggle.
class LibraryHeader extends StatelessWidget {
  const LibraryHeader({
    super.key,
    required this.title,
    required this.panelOpen,
    required this.onTogglePanel,
    this.actions = const [],
  });

  final String title;
  final bool panelOpen;
  final VoidCallback onTogglePanel;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) => WindowDragArea(
    child: Container(
      height: titleBarInset + 12,
      padding: const EdgeInsets.fromLTRB(12, 0, 10, 0),
      child: Row(
        children: [
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              title,
              style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
            ),
          ),
          ...actions,
          QuietIconButton(
            icon: panelOpen ? Icons.view_sidebar : Icons.view_sidebar_outlined,
            tooltip: panelOpen ? 'Hide activity' : 'Show activity',
            onPressed: onTogglePanel,
          ),
        ],
      ),
    ),
  );
}

/// The filters the ledger offers: a label and the kind family it asks for.
const ledgerFilters = <(String, String?)>[
  ('All', null),
  ('Approvals', 'approval'),
  ('Writes', 'effect'),
  ('Grants', 'grant'),
  ('Outbox', 'outbox'),
  ('Connections', 'connection'),
  ('Connectors', 'connector'),
  ('Autonomy', 'autonomy'),
  ('Webhooks', 'webhook'),
  ('Devices', 'device'),
];

/// Library → Activity ledger (tooling spec §8.1): everything tern did or
/// was allowed to do that matters for safety, newest first, with the hash
/// chain's verdict on top. Owner-only.
class LedgerView extends StatefulWidget {
  const LedgerView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  State<LedgerView> createState() => _LedgerViewState();
}

class _LedgerViewState extends State<LedgerView> {
  static const _page = 100;

  String? _kind;
  List<LedgerEntry>? _entries;
  LedgerCheck? _check;
  var _more = false;
  var _loading = false;
  String? _error;

  TernClient get _client => widget.store.client;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load({bool older = false}) async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final before = older ? _entries?.lastOrNull?.seq : null;
      final rows = await _client.ledger(
        kind: _kind,
        before: before,
        limit: _page,
      );
      final check = older ? _check : await _client.verifyLedger();
      if (!mounted) return;
      setState(() {
        _entries = [if (older) ...?_entries, ...rows];
        _check = check;
        _more = rows.length == _page;
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  void _filter(String? kind) {
    _kind = kind;
    _entries = null;
    _load();
  }

  /// Open where the entry happened: its conversation, else its routine,
  /// else its connector's accounts.
  void _open(LedgerEntry e) {
    final s = widget.store;
    if (e.sessionId case final session?) {
      s.openThread(session);
    } else if (e.routineId != null) {
      s.openRoutines();
    } else if (e.family == 'connection' && s.canManageConnections) {
      s.openConnections();
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final entries = _entries;
    return Column(
      children: [
        LibraryHeader(
          title: 'Activity ledger',
          panelOpen: widget.panelOpen,
          onTogglePanel: widget.onTogglePanel,
          actions: [
            QuietIconButton(
              icon: Icons.refresh,
              tooltip: 'Refresh and verify',
              onPressed: _loading ? null : _load,
            ),
          ],
        ),
        const Hairline(),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.fromLTRB(24, 16, 24, 24),
            children: [
              if (_check case final check?) ChainBanner(check: check),
              const SizedBox(height: 12),
              UnderlineTabs(
                tabs: [
                  for (final (label, kind) in ledgerFilters)
                    UnderlineTab(
                      label: label,
                      selected: _kind == kind,
                      onTap: () => _filter(kind),
                    ),
                ],
              ),
              const SizedBox(height: 12),
              if (_error case final e?)
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              if (entries == null && _error == null)
                const Padding(
                  padding: EdgeInsets.all(24),
                  child: Center(child: CircularProgressIndicator()),
                )
              else if (entries != null && entries.isEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 24),
                  child: Text(
                    _kind == null
                        ? 'Nothing yet. Approvals, grants, writes, '
                              'connections and setting changes are recorded '
                              'here as they happen.'
                        : 'Nothing of this kind yet.',
                    style: TextStyle(color: c.muted),
                  ),
                ),
              for (final e in entries ?? const <LedgerEntry>[])
                LedgerRow(entry: e, onOpen: () => _open(e)),
              if (_more)
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton(
                    onPressed: _loading ? null : () => _load(older: true),
                    child: const Text('Older'),
                  ),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

/// The chain's verdict: intact, or where it was changed outside tern.
class ChainBanner extends StatelessWidget {
  const ChainBanner({super.key, required this.check});

  final LedgerCheck check;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final ok = check.ok;
    final color = ok ? c.ok : c.error;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: 2),
          child: Icon(
            ok ? Icons.verified_outlined : Icons.gpp_bad_outlined,
            size: 16,
            color: color,
          ),
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                ok
                    ? 'Intact · ${check.entries} '
                          '${check.entries == 1 ? 'entry' : 'entries'}'
                    : 'Changed outside tern at entry #${check.brokenAt}',
                style: TextStyle(fontWeight: FontWeight.w600, color: color),
              ),
              SelectableText(
                ok
                    ? 'Each entry is chained to the one before by its '
                          'hash. Head ${_short(check.head)}'
                    : '${check.reason ?? ''}. Entries before it are '
                          'intact.',
                style: TextStyle(fontSize: 12, color: c.muted),
              ),
            ],
          ),
        ),
      ],
    );
  }

  static String _short(String h) =>
      h.length > 16 ? '${h.substring(0, 16)}…' : h;
}

IconData ledgerIcon(String kind) => switch (kind.split('.').first) {
  'approval' => Icons.how_to_reg_outlined,
  'effect' => Icons.edit_note,
  'grant' => Icons.key_outlined,
  'outbox' => Icons.outbox_outlined,
  'connection' => Icons.link,
  'connector' => Icons.extension_outlined,
  'autonomy' || 'egress' || 'sandbox' => Icons.tune,
  'webhook' => Icons.webhook_outlined,
  'device' => Icons.devices_outlined,
  _ => Icons.circle_outlined,
};

/// `owner` → you; `agent:tern:writer` → the agent's id.
String ledgerActor(String actor) => switch (actor) {
  'owner' => 'you',
  'tern' => 'tern',
  _ when actor.startsWith('agent:') => actor.substring(6),
  _ => actor,
};

class LedgerRow extends StatelessWidget {
  const LedgerRow({super.key, required this.entry, this.onOpen});

  final LedgerEntry entry;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final e = entry;
    final t = e.at;
    String two(int n) => n.toString().padLeft(2, '0');
    final when =
        '${t.year}-${two(t.month)}-${two(t.day)} ${two(t.hour)}:${two(t.minute)}';
    final opens = e.sessionId != null || e.routineId != null;
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      onTap: opens ? onOpen : null,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 7, horizontal: 4),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(ledgerIcon(e.kind), size: 16, color: c.muted),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(e.summary, style: const TextStyle(fontSize: 13)),
                  const SizedBox(height: 2),
                  Text(
                    [
                      when,
                      e.kind,
                      'by ${ledgerActor(e.actor)}',
                      ?e.connector,
                      '#${e.seq}',
                    ].join(' · '),
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                ],
              ),
            ),
            if (opens) Icon(Icons.chevron_right, size: 16, color: c.muted),
          ],
        ),
      ),
    );
  }
}
