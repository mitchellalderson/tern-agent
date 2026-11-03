import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
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
                    tooltip: 'New conversation (⌘N)',
                    onPressed: store.newConversation,
                  ),
                ),
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 6, 12, 10),
            child: _SearchField(
              controller: _search,
              onChanged: (_) => setState(() {}),
            ),
          ),
          _AgentRow(store: store),
          const SizedBox(height: 6),
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
                      selected: s.id == store.selected,
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
                      style: Theme.of(context).textTheme.bodySmall?.copyWith(
                        color: context.colors.accent,
                      ),
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
        transcript?.items.any((i) => i is ApprovalItem && i.outcome == null) ??
        false;
    final running = transcript?.activeRun != null;
    final icon = switch (session.source) {
      'cron' => Icons.schedule,
      'telegram' => Icons.send_outlined,
      _ => null,
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
                    message: 'Waiting for your approval',
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
