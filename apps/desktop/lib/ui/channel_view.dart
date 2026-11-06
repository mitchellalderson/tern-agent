import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../state/workspaces.dart';
import '../theme.dart';
import 'agent_face.dart';
import 'channel_items.dart' show SuggestionActions;
import 'conversation.dart';
import 'feed.dart';
import 'fleet_status.dart';
import 'routines.dart';
import 'items.dart';
import 'manage.dart';
import 'reasoning.dart';
import 'widgets.dart';
import 'workspace_ui.dart';

/// A topic channel's timeline (docs/channels-design.md §5.2): its posts,
/// who is working, and a composer that posts to the channel.
class ChannelView extends StatefulWidget {
  const ChannelView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  /// The post [p] replies to, to quote above it: unless it's the one
  /// right before it in the timeline (DECISIONS.md "Flat conversations
  /// everywhere": replies are posts, quoted, never threads).
  static Post? quoted(Post p, Post? before, List<Post> all) {
    final to = p.replyTo;
    if (to == null || before?.id == to) return null;
    return all.where((x) => x.id == to).firstOrNull;
  }

  @override
  State<ChannelView> createState() => _ChannelViewState();
}

class _ChannelViewState extends State<ChannelView> {
  final _window = FeedWindow();
  String? _open;
  var _loadingEarlier = false;

  TernStore get store => widget.store;
  bool get panelOpen => widget.panelOpen;
  VoidCallback get onTogglePanel => widget.onTogglePanel;

  @override
  void initState() {
    super.initState();
    _window.addListener(_changed);
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _window
      ..removeListener(_changed)
      ..dispose();
    super.dispose();
  }

  /// Widen the window; at its end, fetch the posts before what is loaded.
  Future<void> _showEarlier(int hidden, String channel) async {
    if (hidden > 0) {
      _window.showEarlier();
      return;
    }
    setState(() => _loadingEarlier = true);
    await store.loadOlderPosts(channel);
    if (!mounted) return;
    _loadingEarlier = false;
    _window.showEarlier();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final ch = store.openChannel;
    if (ch == null) return const SizedBox.shrink();
    if (ch.id != _open) {
      if (_open != null) _window.reset();
      _open = ch.id;
    }
    // One flat timeline: every post, in order; only the window is built.
    final everything = store.posts[ch.id] ?? const <Post>[];
    final hiddenEarlier = _window.hiddenEarlier(everything.length);
    final shown = _window.slice(everything);
    // One live line per member at work here (channels-design §5.2).
    final lines = _window.held
        ? const <({String runId, String author, String doing})>[]
        : store.liveIn(ch.id);
    // Approvals and secret requests wait here, as cards in place of their
    // posts; one whose post hasn't arrived yet goes at the bottom.
    final placed = {
      for (final p in shown) ?p.approvalId,
      for (final p in shown) ?p.secretRequestId,
    };
    final loose = [
      for (final i in _window.held ? const <Item>[] : store.waitingCards(ch.id))
        if (!placed.contains(switch (i) {
          ApprovalItem(:final approval) => approval.id,
          SecretRequestItem(:final request) => request.id,
          _ => null,
        }))
          i,
    ];
    final extra = lines.length + loose.length;
    return Column(
      children: [
        _ChannelHeader(
          store: store,
          channel: ch,
          panelOpen: panelOpen,
          onTogglePanel: onTogglePanel,
        ),
        const Hairline(),
        if (store.workspaces case final api?)
          WorkspaceBanner(api: api, target: WorkspaceTarget.channel(ch.id)),
        if (store.homeOpen && store.hasAgents)
          FleetStrip(
            store: store,
            padding: const EdgeInsets.fromLTRB(20, 4, 20, 0),
          ),
        Expanded(
          child: shown.isEmpty && extra == 0
              ? _EmptyChannel(channel: ch)
              : FeedList(
                  window: _window,
                  count: shown.length + extra,
                  total: everything.length,
                  noun: 'posts',
                  hiddenEarlier: hiddenEarlier,
                  moreEarlier: store.postsMore[ch.id] ?? false,
                  loadingEarlier: _loadingEarlier,
                  onShowEarlier: () => _showEarlier(hiddenEarlier, ch.id),
                  keyFor: (i) => i < lines.length
                      ? ValueKey('line:${lines[lines.length - 1 - i].runId}')
                      : i < extra
                      ? itemKey(loose[extra - 1 - i])
                      : ValueKey(
                          'post:${shown[shown.length - 1 - (i - extra)].id}',
                        ),
                  itemBuilder: (context, i) => Center(
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 760),
                      child: Padding(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 24,
                          vertical: 6,
                        ),
                        child: i < lines.length
                            ? MemberLine(
                                line: lines[lines.length - 1 - i],
                                store: store,
                              )
                            : i < extra
                            ? ItemView(item: loose[extra - 1 - i], store: store)
                            : widget._timelinePost(
                                shown,
                                shown.length - 1 - (i - extra),
                              ),
                      ),
                    ),
                  ),
                ),
        ),
        if (store.error case final e?)
          Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 8),
            child: Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
          ),
        Composer(store: store),
      ],
    );
  }
}

extension on ChannelView {
  Widget _timelinePost(List<Post> shown, int i) {
    final post = shown[i];
    if (post.isRequest) {
      // Waiting: the card itself, answered here. Answered: how it ended.
      final card = post.needsYou ? store.requestCard(post) : null;
      return card != null
          ? ItemView(item: card, store: store)
          : NoteLine(post: post, icon: Icons.check_circle_outline);
    }
    return TimelinePost(
      post: post,
      store: store,
      quote: ChannelView.quoted(post, i > 0 ? shown[i - 1] : null, shown),
    );
  }
}

/// "research-1 · Running http_fetch": a member working in the channel.
/// When the model is reasoning, a chevron beside the words opens a quiet
/// box that streams it.
class MemberLine extends StatefulWidget {
  const MemberLine({super.key, required this.line, required this.store});

  final ({String runId, String author, String doing}) line;
  final TernStore store;

  @override
  State<MemberLine> createState() => _MemberLineState();
}

class _MemberLineState extends State<MemberLine> {
  var _open = false;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final line = widget.line;
    final store = widget.store;
    final reasoning = store.reasoningOfRun(line.runId);
    final words = Text(
      '${store.authorName(line.author, line.author)} · ${line.doing}',
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: TextStyle(fontSize: 12.5, color: c.muted),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            authorAvatar(
              store,
              authorId: line.author,
              name: line.author,
              size: 18,
            ),
            const SizedBox(width: 8),
            SizedBox(
              width: 10,
              height: 10,
              child: CircularProgressIndicator(
                strokeWidth: 1.5,
                color: c.muted,
              ),
            ),
            const SizedBox(width: 8),
            if (reasoning == null)
              Flexible(child: words)
            else
              Flexible(
                child: InkWell(
                  key: const ValueKey('reasoning-toggle'),
                  onTap: () => setState(() => _open = !_open),
                  borderRadius: BorderRadius.circular(6),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Flexible(child: words),
                      const SizedBox(width: 2),
                      ReasoningChevron(open: _open),
                    ],
                  ),
                ),
              ),
          ],
        ),
        if (reasoning != null && _open) ...[
          const SizedBox(height: 8),
          Padding(
            padding: const EdgeInsets.only(left: 26),
            child: ReasoningBox(reasoning: reasoning),
          ),
        ],
      ],
    );
  }
}

/// "Waiting for you in #renovation": a thread's run asks in the
/// conversation it's in, which isn't the one open.
class WaitingIn extends StatelessWidget {
  const WaitingIn({
    super.key,
    required this.store,
    required this.channelId,
    required this.count,
  });

  final TernStore store;
  final String channelId;
  final int count;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final where = store.conversationName(channelId);
    return Material(
      color: c.warn.withValues(alpha: 0.1),
      child: InkWell(
        onTap: () => store.openConversation(channelId),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 8),
          child: Row(
            children: [
              Icon(Icons.pan_tool_alt_outlined, size: 15, color: c.warn),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  count == 1
                      ? 'Waiting for you in $where'
                      : '$count things waiting for you in $where',
                  style: const TextStyle(fontSize: 12.5),
                ),
              ),
              Text(
                'Go to $where',
                style: TextStyle(fontSize: 12.5, color: c.link),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ChannelHeader extends StatelessWidget {
  const _ChannelHeader({
    required this.store,
    required this.channel,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final Channel channel;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    // Its agents, lead first (agent profiles §5.1); without profiles, who
    // has posted lately, newest first.
    final agents = channel.agents == null || !store.hasAgents
        ? null
        : store.membersOf(channel);
    final members = <Post>[];
    for (final p in (store.posts[channel.id] ?? const <Post>[]).reversed) {
      if (agents != null) break;
      if (!p.mine &&
          !p.isNote &&
          !members.any((m) => m.authorId == p.authorId)) {
        members.add(p);
      }
      if (members.length == 4) break;
    }
    final main = channel.mainSessionId;
    return WindowDragArea(
      child: Container(
        height: titleBarInset + 12,
        padding: const EdgeInsets.fromLTRB(20, 0, 10, 0),
        alignment: Alignment.center,
        child: Row(
          children: [
            Text(
              '#',
              style: TextStyle(
                fontSize: 17,
                color: c.muted,
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: 6),
            Expanded(
              child: Row(
                children: [
                  Flexible(
                    child: Text(
                      channel.title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (channel.instructions case final i?) ...[
                    const SizedBox(width: 6),
                    Tooltip(
                      message: i,
                      child: Icon(Icons.rule, size: 15, color: c.muted),
                    ),
                  ],
                ],
              ),
            ),
            if (agents != null)
              MemberAvatars(store: store, channel: channel, agents: agents)
            else
              for (final m in members)
                Padding(
                  padding: const EdgeInsets.only(right: 3),
                  child: Tooltip(
                    message: store.authorName(m.authorId, m.author),
                    child: authorAvatar(
                      store,
                      authorId: m.authorId,
                      name: m.author,
                      size: 20,
                    ),
                  ),
                ),
            const SizedBox(width: 6),
            if (main != null)
              if (store.usage[main]
                  case Usage(
                    :final contextTokens?,
                    :final contextWindow?,
                    :final compactAt,
                  )
                  when contextWindow > 0)
                ContextMeter(
                  used: contextTokens,
                  window: contextWindow,
                  compactAt: compactAt,
                ),
            if (store.workspaces case final api?)
              WorkspaceChip(
                api: api,
                target: WorkspaceTarget.channel(channel.id),
              ),
            _RoutinesButton(store: store, channel: channel),
            QuietIconButton(
              icon: Icons.tune,
              tooltip: 'Channel settings',
              onPressed: () => showChannelSettings(context, store, channel),
            ),
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
    );
  }
}

/// The routines that post to a topic channel, from its header: a clock
/// with their count.
class _RoutinesButton extends StatelessWidget {
  const _RoutinesButton({required this.store, required this.channel});

  final TernStore store;
  final Channel channel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final n = store.schedulesIn(channel.id).length;
    return Tooltip(
      message: n == 0 ? 'Routines' : '$n routine${n == 1 ? '' : 's'} post here',
      child: TextButton.icon(
        key: const ValueKey('channel-routines'),
        onPressed: () => showChannelRoutines(context, store, channel),
        style: TextButton.styleFrom(
          foregroundColor: c.muted,
          padding: const EdgeInsets.symmetric(horizontal: 6),
          minimumSize: const Size(0, 30),
          visualDensity: VisualDensity.compact,
        ),
        icon: const Icon(Icons.schedule, size: 16),
        label: Text(
          n == 0 ? 'Routines' : '$n',
          style: const TextStyle(fontSize: 12.5),
        ),
      ),
    );
  }
}

/// A topic channel's agents in its header, the lead first and marked,
/// then "+ Agent" to add one (agent profiles §5.1).
class MemberAvatars extends StatelessWidget {
  const MemberAvatars({
    super.key,
    required this.store,
    required this.channel,
    required this.agents,
  });

  final TernStore store;
  final Channel channel;
  final List<Agent> agents;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final lead = channel.lead;
    final others = [
      for (final a in store.agents)
        if (!a.archived && !channel.memberIds.contains(a.id)) a,
    ];
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final a in agents)
          Padding(
            padding: const EdgeInsets.only(right: 3),
            child: Tooltip(
              message: a.id == lead
                  ? '${a.name} · lead: answers posts that mention no one'
                  : '${a.name} · answers when @${a.slug} is mentioned',
              child: Stack(
                clipBehavior: Clip.none,
                children: [
                  AgentAvatar.of(a, size: 22),
                  if (a.id == lead)
                    Positioned(
                      right: -3,
                      bottom: -3,
                      child: Container(
                        padding: const EdgeInsets.all(1.5),
                        decoration: BoxDecoration(
                          color: c.background,
                          shape: BoxShape.circle,
                        ),
                        child: Icon(
                          Icons.star_rounded,
                          size: 10,
                          color: c.warn,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
        const SizedBox(width: 4),
        PopupMenuButton<String>(
          tooltip: others.isEmpty
              ? 'Every agent is in #${channel.slug}'
              : 'Add an agent to #${channel.slug}',
          enabled: others.isNotEmpty,
          position: PopupMenuPosition.under,
          onSelected: (id) => store.addToChannel(channel.id, id),
          itemBuilder: (_) => [
            for (final a in others)
              PopupMenuItem(
                value: a.id,
                height: 36,
                child: Row(
                  children: [
                    AgentAvatar.of(a, size: 20),
                    const SizedBox(width: 8),
                    Text(a.name),
                    const SizedBox(width: 6),
                    Text(
                      '@${a.slug}',
                      style: TextStyle(fontSize: 12, color: c.muted),
                    ),
                  ],
                ),
              ),
          ],
          child: Container(
            height: 24,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            alignment: Alignment.center,
            decoration: BoxDecoration(
              border: Border.all(color: c.hairline),
              borderRadius: BorderRadius.circular(12),
            ),
            child: Text(
              '+ Agent',
              style: TextStyle(
                fontSize: 12,
                color: others.isEmpty ? c.muted : c.text,
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// A quiet line from tern in a timeline: "Research joined", "@ops not
/// woken: …".
class NoteLine extends StatelessWidget {
  const NoteLine({
    super.key,
    required this.post,
    this.icon = Icons.info_outline,
  });

  final Post post;
  final IconData icon;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Icon(icon, size: 13, color: c.muted),
          const SizedBox(width: 6),
          Flexible(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 520),
              child: Text(
                post.text,
                textAlign: TextAlign.center,
                style: TextStyle(fontSize: 12, color: c.muted),
              ),
            ),
          ),
          const SizedBox(width: 6),
          Text(
            relativeTime(post.createdAt),
            style: TextStyle(fontSize: 11, color: c.muted),
          ),
        ],
      ),
    );
  }
}

/// `text` with each `@slug` an agent profile has (of `slugs`) turned into
/// a `mention:` link, so the timeline can colour it.
String markMentions(String text, Iterable<String> slugs) {
  final known = slugs.toSet();
  if (known.isEmpty) return text;
  return text.replaceAllMapped(
    RegExp(r'(^|[^A-Za-z0-9._+@-])@([a-z0-9][a-z0-9-]*)', caseSensitive: false),
    (m) {
      final slug = m[2]!.toLowerCase();
      return known.contains(slug) ? '${m[1]}[@${m[2]}](mention:$slug)' : m[0]!;
    },
  );
}

class _EmptyChannel extends StatelessWidget {
  const _EmptyChannel({required this.channel});

  final Channel channel;

  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          '#${channel.slug}',
          style: Theme.of(context).textTheme.titleMedium
              ?.copyWith(fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 6),
        Text(
          channel.agents == null
              ? 'Ask tern something here, or point a routine at this channel.'
              : 'Ask something here, or @mention an agent. Routines can post '
                    'here too.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    ),
  );
}

/// One post in a channel's timeline.
class TimelinePost extends StatelessWidget {
  const TimelinePost({
    super.key,
    required this.post,
    required this.store,
    this.quote,
  });

  final Post post;
  final TernStore store;

  /// The post it replies to, when that isn't right above it.
  final Post? quote;

  @override
  Widget build(BuildContext context) {
    if (post.isNote) return NoteLine(post: post);
    final c = context.colors;
    final theme = Theme.of(context);
    final slugs = [
      for (final id in post.mentions)
        if (store.agentById(id) case final a?) a.slug,
    ];
    final tag = switch (post.authorKind) {
      'routine' => (Icons.schedule, 'routine'),
      'subagent' => (Icons.subdirectory_arrow_right, 'under a parent agent'),
      _ => null,
    };
    final session = post.sessionId;
    final row = Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        post.mine
            ? CircleAvatar(
                radius: 13,
                backgroundColor: c.background,
                child: Icon(Icons.person, size: 15, color: c.muted),
              )
            : authorAvatar(
                store,
                authorId: post.authorId,
                name: post.author,
                size: 26,
              ),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Text(
                    store.authorName(post.authorId, post.author),
                    style: const TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  if (post.tainted) ...[
                    const SizedBox(width: 5),
                    const TaintMark(),
                  ],
                  if (tag case (final icon, final words)) ...[
                    const SizedBox(width: 5),
                    Tooltip(
                      message: words,
                      child: Icon(icon, size: 13, color: c.muted),
                    ),
                  ],
                  const SizedBox(width: 8),
                  Text(
                    relativeTime(post.createdAt),
                    // `muted` misses 4.5:1 on the user surface.
                    style: TextStyle(
                      fontSize: 11.5,
                      color: post.mine ? c.text : c.muted,
                    ),
                  ),
                  if (post.needsYou) ...[
                    const SizedBox(width: 8),
                    const NeedsYouTag(),
                  ],
                ],
              ),
              if (quote case final q?) ...[
                const SizedBox(height: 2),
                QuoteLine(post: q, store: store),
              ],
              if (store.reasoningOfPost(post) case final r?) ...[
                const SizedBox(height: 2),
                ReasoningNote(reasoning: r),
              ],
              const SizedBox(height: 3),
              MarkdownBody(
                data: markMentions(post.text, slugs),
                selectable: true,
                onTapLink: (_, href, _) => openTernLink(context, store, href),
                styleSheet: MarkdownStyleSheet.fromTheme(theme).copyWith(
                  p: theme.textTheme.bodyMedium,
                  a: TextStyle(color: c.link, fontWeight: FontWeight.w600),
                ),
              ),
              for (final id in routineLinksIn(post.text))
                RoutineCard(item: RoutineItem(id), store: store),
              if (post.suggestion case final sg?)
                SuggestionActions(suggestion: sg, store: store),
              Row(
                children: [
                  // An outbox item: decide it here (topic channels have no
                  // outbox cards of their own).
                  if (post.needsYou && post.outboxId != null) ...[
                    if (store.outbox
                            .where((o) => o.id == post.outboxId && o.pending)
                            .firstOrNull
                        case final item?) ...[
                      _Action(
                        label: 'Approve',
                        onTap: () => store.decideOutbox(item, approve: true),
                      ),
                      _Action(
                        label: 'Reject',
                        onTap: () => store.decideOutbox(item, approve: false),
                      ),
                    ],
                  ] else if (post.suggestion != null)
                    const SizedBox.shrink()
                  else if (post.needsYou && !post.isRequest)
                    _Action(label: 'Done', onTap: () => store.resolvePost(post))
                  else if (post.kind == 'needs_you')
                    const _Action(label: 'Done ✓', onTap: null),
                  // To read beside the timeline: the run behind an agent's
                  // post (tool steps and all), or a moved-in conversation.
                  if (session != null && post.kind == 'moved')
                    _Action(
                      label: 'Read conversation',
                      onTap: () => store.openThread(session),
                    )
                  else if (session != null && !post.mine)
                    _Action(
                      label: 'See work',
                      onTap: () => store.openThread(session),
                    ),
                ],
              ),
            ],
          ),
        ),
      ],
    );
    // The user's posts get the same flat full-width tint as a conversation's
    // user messages (brand guide 5); agent posts stay on the page.
    if (!post.mine) return row;
    return Container(
      key: const Key('user-post'),
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
      decoration: BoxDecoration(
        color: c.userBubble,
        borderRadius: BorderRadius.circular(8),
      ),
      child: row,
    );
  }
}

/// "↪ replying to you: …": the post a reply answers, one line, when it
/// isn't right above.
class QuoteLine extends StatelessWidget {
  const QuoteLine({super.key, required this.post, required this.store});

  final Post post;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final who = post.mine
        ? 'you'
        : store.authorName(post.authorId, post.author);
    final text = post.text.replaceAll(RegExp(r'\s+'), ' ').trim();
    return Row(
      children: [
        Icon(Icons.subdirectory_arrow_right, size: 13, color: c.muted),
        const SizedBox(width: 4),
        Flexible(
          child: Text(
            'replying to $who: “$text”',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 12, color: c.muted),
          ),
        ),
      ],
    );
  }
}

/// Marks a post that waits on the user.
class NeedsYouTag extends StatelessWidget {
  const NeedsYouTag({super.key, this.where});

  /// For a pointer: the channel it came from.
  final String? where;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: c.warn.withValues(alpha: 0.15),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        where == null ? 'needs you' : 'needs you · $where',
        style: TextStyle(fontSize: 11, color: c.warn),
      ),
    );
  }
}

class _Action extends StatelessWidget {
  const _Action({required this.label, required this.onTap});

  final String label;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) => InkWell(
    borderRadius: BorderRadius.circular(6),
    onTap: onTap,
    child: Padding(
      padding: const EdgeInsets.fromLTRB(0, 4, 12, 2),
      child: Text(
        label,
        style: TextStyle(fontSize: 12, color: context.colors.muted),
      ),
    ),
  );
}

/// `[a-z0-9][a-z0-9-]{0,31}`, as the daemon wants it.
String slugFor(String title) {
  final s = title
      .toLowerCase()
      .replaceAll(RegExp(r'[^a-z0-9]+'), '-')
      .replaceAll(RegExp(r'^-+|-+$'), '');
  return s.length > 32 ? s.substring(0, 32).replaceAll(RegExp(r'-+$'), '') : s;
}

/// The + beside CHANNELS.
Future<void> showNewChannel(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => ChannelDialog(store: store),
    );

Future<void> showChannelSettings(
  BuildContext context,
  TernStore store,
  Channel channel,
) => showDialog<void>(
  context: context,
  builder: (_) => ChannelDialog(store: store, editing: channel),
);

/// Create a topic channel, or change one's title and instructions.
class ChannelDialog extends StatefulWidget {
  const ChannelDialog({super.key, required this.store, this.editing});

  final TernStore store;
  final Channel? editing;

  @override
  State<ChannelDialog> createState() => _ChannelDialogState();
}

class _ChannelDialogState extends State<ChannelDialog> {
  late final _title = TextEditingController(text: widget.editing?.title);
  late final _instructions = TextEditingController(
    text: widget.editing?.instructions,
  );
  late String? _lead = widget.editing?.agents == null
      ? null
      : widget.editing!.lead;
  late bool _wakes = widget.editing?.agents?.agentWakes ?? false;
  late bool _decide = widget.editing?.agents?.letAgentsDecide ?? false;
  late final _slugCtl = TextEditingController(text: widget.editing?.slug);
  String? _error;
  var _busy = false;

  /// The folder chosen for a new channel; null = the default.
  String? _folder;

  @override
  void initState() {
    super.initState();
    _title.addListener(() => setState(() {}));
    _slugCtl.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _title.dispose();
    _slugCtl.dispose();
    _instructions.dispose();
    super.dispose();
  }

  String get _slug => widget.editing == null
      ? slugFor(_title.text)
      : _slugCtl.text.trim().replaceFirst('#', '');

  Future<void> _save() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final editing = widget.editing;
    final err = editing == null
        ? await widget.store.createChannel(
            slug: _slug,
            title: _title.text.trim(),
            instructions: _instructions.text.trim().isEmpty
                ? null
                : _instructions.text.trim(),
            workspace: _folder,
          )
        : await widget.store.updateChannel(
            editing.id,
            slug: _slug == editing.slug ? null : _slug,
            title: _title.text.trim(),
            instructions: _instructions.text.trim(),
            lead: _lead == editing.lead ? null : _lead,
            agentWakes:
                editing.agents == null || _wakes == editing.agents!.agentWakes
                ? null
                : _wakes,
            letAgentsDecide:
                editing.agents == null ||
                    _decide == editing.agents!.letAgentsDecide
                ? null
                : _decide,
          );
    if (!mounted) return;
    if (err == null) {
      if (mounted) Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final editing = widget.editing;
    final folders = editing == null ? widget.store.workspaces : null;
    final labels = Theme.of(context).textTheme.labelSmall;
    final ok = _slug.isNotEmpty && !_busy;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                editing == null ? 'New channel' : '#${editing.slug}',
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 4),
              Text(
                'A channel is a topic: its own timeline, the same memory. '
                'Routines can post their results here.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 16),
              Text('Name', style: labels),
              const SizedBox(height: 6),
              TextField(
                controller: _title,
                autofocus: editing == null,
                decoration: InputDecoration(
                  hintText: 'Finances',
                  isDense: true,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                  helperText: editing == null && _slug.isNotEmpty
                      ? '#$_slug'
                      : null,
                ),
              ),
              if (editing != null && editing.kind == 'topic') ...[
                const SizedBox(height: 14),
                Text('#name', style: labels),
                const SizedBox(height: 6),
                TextField(
                  key: const Key('channel-slug'),
                  controller: _slugCtl,
                  decoration: InputDecoration(
                    prefixText: '#',
                    isDense: true,
                    border: OutlineInputBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                    helperText:
                        'Lowercase letters, digits and dashes. Its posts and '
                        'routines follow the rename.',
                  ),
                ),
              ],
              const SizedBox(height: 14),
              Text('Instructions', style: labels),
              const SizedBox(height: 6),
              TextField(
                controller: _instructions,
                minLines: 2,
                maxLines: 5,
                decoration: InputDecoration(
                  hintText: 'Amounts in EUR. Never move money.',
                  isDense: true,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                  helperText: 'Added to what tern is told for work here.',
                ),
              ),
              if (folders != null) ...[
                const SizedBox(height: 14),
                FolderRow(
                  api: folders,
                  path: _folder,
                  emptyLabel: 'Default folder',
                  note:
                      'Where agents in this channel read and write files. '
                      'You can change it later from the channel header.',
                  onChosen: (p) async {
                    setState(() => _folder = p);
                    return null;
                  },
                  onClear: () => setState(() => _folder = null),
                ),
              ],
              if (editing?.agents case final agents?
                  when widget.store.hasAgents) ...[
                const SizedBox(height: 14),
                Text('Lead', style: labels),
                const SizedBox(height: 6),
                DropdownButtonFormField<String>(
                  initialValue: _lead,
                  isDense: true,
                  decoration: InputDecoration(
                    isDense: true,
                    border: OutlineInputBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                    helperText: 'Answers posts that mention no one.',
                  ),
                  items: [
                    for (final a in widget.store.mentionable(editing!))
                      DropdownMenuItem(
                        value: a.agent.id,
                        child: Row(
                          children: [
                            AgentAvatar.of(a.agent, size: 18),
                            const SizedBox(width: 8),
                            Text(a.agent.name),
                            if (!a.member) ...[
                              const SizedBox(width: 6),
                              Text(
                                'will join',
                                style: TextStyle(
                                  fontSize: 11.5,
                                  color: c.muted,
                                ),
                              ),
                            ],
                          ],
                        ),
                      ),
                  ],
                  onChanged: (v) => setState(() => _lead = v),
                ),
                const SizedBox(height: 6),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  dense: true,
                  value: _wakes,
                  onChanged: (v) => setState(() => _wakes = v),
                  title: const Text('Agents can wake each other'),
                  subtitle: Text(
                    _wakes
                        ? "An agent's @mention starts the agent it names: at "
                              'most ${agents.maxHops} in a row from your '
                              'message, and ${agents.wakesPerHour} an hour.'
                        : "Agents' @mentions are shown but start nothing; "
                              'yours always do.',
                  ),
                ),
                SwitchListTile(
                  key: const Key('let-agents-decide'),
                  contentPadding: EdgeInsets.zero,
                  dense: true,
                  value: _decide,
                  onChanged: (v) => setState(() => _decide = v),
                  title: const Text('Let agents decide'),
                  subtitle: Text(
                    _decide
                        ? 'A post that mentions no one goes to the members it '
                              'fits, picked by one cheap model call. If that '
                              'fails, the lead answers.'
                        : 'Off: a post that mentions no one always goes to '
                              'the lead.',
                  ),
                ),
              ],
              if (_error case final e?) ...[
                const SizedBox(height: 10),
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              ],
              const SizedBox(height: 16),
              Row(
                children: [
                  if (editing != null)
                    TextButton(
                      onPressed: _busy
                          ? null
                          : () async {
                              Navigator.of(context).pop();
                              await widget.store.archiveChannel(editing.id);
                            },
                      child: Text('Archive', style: TextStyle(color: c.error)),
                    ),
                  if (editing != null && editing.kind == 'topic')
                    TextButton(
                      key: const Key('delete-channel'),
                      onPressed: _busy
                          ? null
                          : () async {
                              final nav = Navigator.of(context);
                              await showDeleteChannel(
                                context,
                                widget.store,
                                editing,
                              );
                              if (mounted &&
                                  !widget.store.channels.any(
                                    (x) => x.id == editing.id,
                                  )) {
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
            ],
          ),
        ),
      ),
    );
  }
}
