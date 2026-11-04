import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'agent_sheet.dart';
import 'channel_view.dart';
import 'providers.dart';
import 'widgets.dart';

class Sidebar extends StatefulWidget {
  const Sidebar({super.key, required this.store});

  final TernStore store;

  @override
  State<Sidebar> createState() => _SidebarState();
}

class _SidebarState extends State<Sidebar> {
  final _search = TextEditingController();

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final q = _search.text.trim().toLowerCase();
    final sessions = store.sessions
        .where((s) => q.isEmpty || (s.title ?? '').toLowerCase().contains(q))
        .toList();
    return Container(
      color: c.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          WindowDragArea(
            child: SizedBox(
              height: titleBarInset + 4,
              child: Align(
                alignment: Alignment.bottomRight,
                child: Padding(
                  padding: const EdgeInsets.only(right: 8),
                  child: QuietIconButton(
                    icon: Icons.edit_square,
                    tooltip: store.hasChannels
                        ? 'New topic (⌘N)'
                        : 'New conversation (⌘N)',
                    onPressed: store.newConversation,
                  ),
                ),
              ),
            ),
          ),
          if (!store.hasChannels)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 6, 12, 10),
              child: _SearchField(
                controller: _search,
                onChanged: (_) => setState(() {}),
              ),
            )
          else
            const SizedBox(height: 10),
          // With agent profiles, Home carries tern's avatar instead.
          if (!store.hasAgents) ...[
            _AgentRow(store: store),
            const SizedBox(height: 6),
          ],
          if (store.hasChannels) ...[
            const SizedBox(height: 4),
            _HomeRow(store: store),
            if (store.hasAgents) ...agentsSection(context, store),
            ...channelsSection(context, store),
          ],
          if (store.hasMcp || store.canManageConnections)
            _Library(store: store),
          // With channels, conversations live in Home's History.
          if (store.hasChannels)
            const Spacer()
          else
            Expanded(
              child: ListView(
                padding: const EdgeInsets.symmetric(horizontal: 8),
                children: [
                  for (final group in _group(sessions)) ...[
                    Padding(
                      padding: const EdgeInsets.fromLTRB(10, 14, 10, 4),
                      child: Text(
                        group.$1.toUpperCase(),
                        style: Theme.of(context).textTheme.labelSmall,
                      ),
                    ),
                    for (final s in group.$2)
                      _SessionRow(
                        session: s,
                        selected: s.id == store.selected && store.page == null,
                        transcript: store.transcripts[s.id],
                        onTap: () => store.open(s.id),
                      ),
                  ],
                  if (sessions.isEmpty && store.link == Link.online)
                    Padding(
                      padding: const EdgeInsets.all(12),
                      child: Text(
                        q.isEmpty ? 'No conversations yet.' : 'No matches.',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ),
                ],
              ),
            ),
          const Hairline(),
          _StatusFooter(store: store),
        ],
      ),
    );
  }

  static List<(String, List<SessionItem>)> _group(List<SessionItem> all) {
    final now = DateTime.now();
    final today = DateTime(now.year, now.month, now.day);
    final groups = <String, List<SessionItem>>{};
    for (final s in all) {
      final label = !s.updatedAt.isBefore(today)
          ? 'Today'
          : today.difference(s.updatedAt).inDays < 7
          ? 'This week'
          : 'Older';
      (groups[label] ??= []).add(s);
    }
    return [for (final e in groups.entries) (e.key, e.value)];
  }
}

/// ⌂ Home: the direct line to the agent, with what's waiting for you.
class _HomeRow extends StatelessWidget {
  const _HomeRow({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final selected = store.homeOpen && store.page == null;
    final waiting = store.pendingApprovals + store.pendingSecrets;
    final running = store.transcripts[TernStore.homeKey]?.activeRun != null;
    final tern = store.defaultAgent;
    void edit() => showAgentSheet(context, store, agent: tern);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Material(
        color: selected ? c.selection : Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        child: GestureDetector(
          // Home is the default agent's DM: right-click edits it.
          onSecondaryTapUp: tern == null
              ? null
              : (d) async {
                  final pick = await showMenu<String>(
                    context: context,
                    position: RelativeRect.fromLTRB(
                      d.globalPosition.dx,
                      d.globalPosition.dy,
                      d.globalPosition.dx,
                      d.globalPosition.dy,
                    ),
                    items: [
                      PopupMenuItem(
                        value: 'edit',
                        child: Text('Edit ${tern.name}'),
                      ),
                    ],
                  );
                  if (pick == 'edit' && context.mounted) edit();
                },
          child: InkWell(
            borderRadius: BorderRadius.circular(8),
            onTap: () => store.openHome(),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
              child: Row(
                children: [
                  if (tern != null)
                    AgentAvatar.of(tern, size: 18)
                  else
                    Icon(Icons.home_outlined, size: 15, color: c.muted),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      'Home',
                      style: TextStyle(
                        fontSize: 13.5,
                        fontWeight: selected
                            ? FontWeight.w500
                            : FontWeight.w400,
                      ),
                    ),
                  ),
                  if (waiting > 0)
                    Tooltip(
                      message: '$waiting waiting for you',
                      child: Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 6,
                          vertical: 1,
                        ),
                        decoration: BoxDecoration(
                          color: c.warn,
                          borderRadius: BorderRadius.circular(8),
                        ),
                        child: Text(
                          '$waiting',
                          style: const TextStyle(
                            fontSize: 11,
                            color: Colors.white,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    )
                  else if (running)
                    SizedBox(
                      width: 10,
                      height: 10,
                      child: CircularProgressIndicator(
                        strokeWidth: 1.5,
                        color: c.muted,
                      ),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// AGENTS (agent profiles §7.2): each agent other than the default, whose
/// DM is Home. A row opens its DM; a badge counts what waits for you, a
/// spinner shows it working, a dot new routine posts; + makes one.
List<Widget> agentsSection(BuildContext context, TernStore store) {
  final c = context.colors;
  return [
    Padding(
      padding: const EdgeInsets.fromLTRB(18, 12, 10, 2),
      child: Row(
        children: [
          Expanded(
            child: Text(
              'AGENTS',
              style: Theme.of(context).textTheme.labelSmall,
            ),
          ),
          SizedBox(
            height: 24,
            child: QuietIconButton(
              icon: Icons.add,
              size: 15,
              tooltip: 'New agent',
              onPressed: () => showAgentSheet(context, store),
            ),
          ),
        ],
      ),
    ),
    for (final a in store.otherAgents) _AgentListRow(store: store, agent: a),
    if (store.otherAgents.isEmpty)
      Padding(
        padding: const EdgeInsets.fromLTRB(18, 2, 18, 4),
        child: Text(
          'Agents with their own job, chat and memory.',
          style: Theme.of(context).textTheme.bodySmall
              ?.copyWith(color: c.muted),
        ),
      ),
  ];
}

class _AgentListRow extends StatelessWidget {
  const _AgentListRow({required this.store, required this.agent});

  final TernStore store;
  final Agent agent;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final a = agent;
    final selected =
        store.channel != null &&
        store.channel == a.mainChannelId &&
        store.page == null;
    final waiting = store.waitingFor(a);
    final dm = store.dmOf(a);
    final unread = dm != null && store.unread(dm);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
      child: Material(
        color: selected ? c.selection : Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        child: InkWell(
          borderRadius: BorderRadius.circular(8),
          onTap: () => store.openAgent(a),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
            child: Row(
              children: [
                AgentAvatar.of(a, size: 18),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    a.name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 13.5,
                      fontWeight: unread || selected
                          ? FontWeight.w600
                          : FontWeight.w400,
                    ),
                  ),
                ),
                if (waiting > 0)
                  WaitingBadge(count: waiting)
                else if (store.agentBusy(a))
                  SizedBox(
                    width: 10,
                    height: 10,
                    child: CircularProgressIndicator(
                      strokeWidth: 1.5,
                      color: c.muted,
                    ),
                  )
                else if (unread)
                  Tooltip(
                    message: 'New posts',
                    child: _Dot(color: c.accent),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// CHANNELS: the topic channels, with an unread dot or a spinner, and +
/// to make one.
List<Widget> channelsSection(BuildContext context, TernStore store) {
  final c = context.colors;
  return [
    Padding(
      padding: const EdgeInsets.fromLTRB(18, 12, 10, 2),
      child: Row(
        children: [
          Expanded(
            child: Text(
              'CHANNELS',
              style: Theme.of(context).textTheme.labelSmall,
            ),
          ),
          SizedBox(
            height: 24,
            child: QuietIconButton(
              icon: Icons.add,
              size: 15,
              tooltip: 'New channel',
              onPressed: () => showNewChannel(context, store),
            ),
          ),
        ],
      ),
    ),
    for (final ch in store.topics)
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 1),
        child: Material(
          color: store.channel == ch.id && store.page == null
              ? c.selection
              : Colors.transparent,
          borderRadius: BorderRadius.circular(8),
          child: InkWell(
            borderRadius: BorderRadius.circular(8),
            onTap: () => store.openChannelView(ch.id),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
              child: Row(
                children: [
                  Text('#', style: TextStyle(fontSize: 14, color: c.muted)),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      ch.title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 13.5,
                        fontWeight: store.unread(ch)
                            ? FontWeight.w600
                            : FontWeight.w400,
                      ),
                    ),
                  ),
                  if (store.waitingIn(ch.id) case final n when n > 0)
                    WaitingBadge(count: n)
                  else if (store.busy(ch))
                    SizedBox(
                      width: 10,
                      height: 10,
                      child: CircularProgressIndicator(
                        strokeWidth: 1.5,
                        color: c.muted,
                      ),
                    )
                  else if (store.unread(ch))
                    Tooltip(
                      message: 'New posts',
                      child: _Dot(color: c.accent),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    if (store.topics.isEmpty)
      Padding(
        padding: const EdgeInsets.fromLTRB(18, 2, 18, 4),
        child: Text(
          'Topics get their own timeline.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ),
  ];
}

class _SearchField extends StatelessWidget {
  const _SearchField({required this.controller, required this.onChanged});

  final TextEditingController controller;
  final ValueChanged<String> onChanged;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return SizedBox(
      height: 30,
      child: TextField(
        controller: controller,
        onChanged: onChanged,
        style: const TextStyle(fontSize: 13),
        decoration: InputDecoration(
          hintText: 'Search',
          hintStyle: TextStyle(color: c.muted, fontSize: 13),
          prefixIcon: Icon(Icons.search, size: 16, color: c.muted),
          prefixIconConstraints: const BoxConstraints(minWidth: 30),
          isDense: true,
          filled: true,
          fillColor: c.background.withValues(alpha: 0.7),
          contentPadding: const EdgeInsets.symmetric(vertical: 7),
          border: OutlineInputBorder(
            borderRadius: BorderRadius.circular(8),
            borderSide: BorderSide(color: c.hairline),
          ),
          enabledBorder: OutlineInputBorder(
            borderRadius: BorderRadius.circular(8),
            borderSide: BorderSide(color: c.hairline),
          ),
          focusedBorder: OutlineInputBorder(
            borderRadius: BorderRadius.circular(8),
            borderSide: BorderSide(color: c.accent, width: 1.2),
          ),
        ),
      ),
    );
  }
}

/// The one agent until agent profiles land (spec §7.2).
class _AgentRow extends StatelessWidget {
  const _AgentRow({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final model = store.info?.defaultModel;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 18),
      child: Row(
        children: [
          const AgentAvatar(name: 'tern', size: 30),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('tern', style: Theme.of(context).textTheme.titleSmall),
                if (model != null)
                  Text(
                    model,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(context).textTheme.bodySmall,
                  )
                else
                  InkWell(
                    onTap: store.link == Link.online
                        ? () => showProvidersDialog(context, store)
                        : null,
                    child: Text(
                      'Set up a model…',
                      style: Theme.of(context).textTheme.bodySmall
                          ?.copyWith(color: context.colors.accent),
                    ),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// Library (gui-spec §4.2): what the agent can use, opened in the centre
/// column. Hidden against cores without the API for it.
class _Library extends StatelessWidget {
  const _Library({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final selected = store.page == LibraryPage.mcp;
    final down = store.mcpServers.where((s) => s.state is McpUnavailable);
    final reauth =
        store.connectionList?.accounts.where((a) => a.failing).length ?? 0;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(10, 10, 10, 4),
            child: Text(
              'LIBRARY',
              style: Theme.of(context).textTheme.labelSmall,
            ),
          ),
          if (store.hasMcp)
            _LibraryRow(
              icon: Icons.extension_outlined,
              label: 'MCP servers',
              selected: selected,
              onTap: () => store.openMcp(),
              trailing: down.isNotEmpty
                  ? Tooltip(
                      message:
                          '${down.map((s) => s.name).join(', ')} unavailable',
                      child: _Dot(color: c.error),
                    )
                  : store.mcpServers.isNotEmpty
                  ? Text(
                      '${store.mcpServers.length}',
                      style: TextStyle(fontSize: 11.5, color: c.muted),
                    )
                  : null,
            ),
          if (store.canManageConnections)
            _LibraryRow(
              icon: Icons.link,
              label: 'Connections',
              selected: store.page == LibraryPage.connections,
              onTap: store.openConnections,
              trailing: reauth > 0
                  ? Tooltip(
                      message: '$reauth need signing in again',
                      child: _Dot(color: c.warn),
                    )
                  : null,
            ),
        ],
      ),
    );
  }
}

class _LibraryRow extends StatelessWidget {
  const _LibraryRow({
    required this.icon,
    required this.label,
    required this.selected,
    required this.onTap,
    this.trailing,
  });

  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Material(
      color: selected ? c.selection : Colors.transparent,
      borderRadius: BorderRadius.circular(8),
      child: InkWell(
        borderRadius: BorderRadius.circular(8),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
          child: Row(
            children: [
              Icon(icon, size: 14, color: c.muted),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  label,
                  style: TextStyle(
                    fontSize: 13.5,
                    fontWeight: selected ? FontWeight.w500 : FontWeight.w400,
                  ),
                ),
              ),
              ?trailing,
            ],
          ),
        ),
      ),
    );
  }
}

class _SessionRow extends StatelessWidget {
  const _SessionRow({
    required this.session,
    required this.selected,
    required this.transcript,
    required this.onTap,
  });

  final SessionItem session;
  final bool selected;
  final Transcript? transcript;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final waiting =
        transcript?.items.any(
          (i) =>
              (i is ApprovalItem && i.outcome == null) ||
              (i is SecretRequestItem && i.open),
        ) ??
        false;
    final running = transcript?.activeRun != null;
    final icon = switch (session.source) {
      'cron' => Icons.schedule,
      final s => gatewaySource(s)?.icon,
    };
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 1),
      child: Material(
        color: selected ? c.selection : Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        child: InkWell(
          borderRadius: BorderRadius.circular(8),
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            child: Row(
              children: [
                if (icon != null) ...[
                  Icon(icon, size: 13, color: c.muted),
                  const SizedBox(width: 6),
                ],
                Expanded(
                  child: Text(
                    session.title ?? 'Untitled',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 13.5,
                      fontWeight: selected ? FontWeight.w500 : FontWeight.w400,
                    ),
                  ),
                ),
                const SizedBox(width: 6),
                if (waiting)
                  Tooltip(
                    message: 'Waiting for you',
                    child: _Dot(color: c.warn),
                  )
                else if (running)
                  SizedBox(
                    width: 10,
                    height: 10,
                    child: CircularProgressIndicator(
                      strokeWidth: 1.5,
                      color: c.muted,
                    ),
                  )
                else
                  Text(
                    relativeTime(session.updatedAt),
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

/// How many approvals, secret requests and outbox items wait in a
/// conversation: answered there (Home has a pointer to each).
class WaitingBadge extends StatelessWidget {
  const WaitingBadge({super.key, required this.count});

  final int count;

  @override
  Widget build(BuildContext context) => Tooltip(
    message: '$count waiting for you here',
    child: Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: context.colors.warn,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Text(
        '$count',
        style: const TextStyle(
          fontSize: 11,
          color: Colors.white,
          fontWeight: FontWeight.w600,
        ),
      ),
    ),
  );
}

class _Dot extends StatelessWidget {
  const _Dot({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    width: 8,
    height: 8,
    decoration: BoxDecoration(color: color, shape: BoxShape.circle),
  );
}

class _StatusFooter extends StatelessWidget {
  const _StatusFooter({required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final (color, text) = switch (store.link) {
      Link.online => (c.ok, 'Connected · tern ${store.health?.version ?? ''}'),
      Link.connecting => (c.muted, 'Connecting…'),
      Link.offline => (c.warn, 'Reconnecting…'),
    };
    return Padding(
      padding: const EdgeInsets.fromLTRB(18, 4, 8, 6),
      child: Row(
        children: [
          Expanded(
            child: Tooltip(
              message: store.linkError ?? store.client.endpoint.describe(),
              child: Row(
                children: [
                  _Dot(color: color),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      text,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
                ],
              ),
            ),
          ),
          QuietIconButton(
            icon: Icons.key,
            size: 16,
            tooltip: 'Models and API keys (⌘,)',
            onPressed: store.link == Link.online
                ? () => showProvidersDialog(context, store)
                : null,
          ),
        ],
      ),
    );
  }
}
