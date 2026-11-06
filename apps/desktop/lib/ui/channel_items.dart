import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import 'agent_face.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'channel_view.dart' show NeedsYouTag;
import 'markdown_text.dart';
import 'routines.dart';
import 'widgets.dart';

/// Where Home's stream was cut into a new session: a thin line, with the
/// summary that was carried over a click away (docs/channels-design.md §2.6).
class CutDivider extends StatefulWidget {
  const CutDivider({super.key, required this.item, this.onMove});

  final CutItem item;

  /// Moves the topic that starts here to a channel.
  final VoidCallback? onMove;

  /// Why the cut happened, in words.
  static String reason(String? r) => switch (r) {
    'idle' => 'after a quiet spell',
    'day' => 'new day',
    'new_topic' => 'new topic',
    'migrated' => 'earlier conversation',
    _ => '',
  };

  @override
  State<CutDivider> createState() => _CutDividerState();
}

class _CutDividerState extends State<CutDivider> {
  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final cut = widget.item.cut;
    final carry = cut.carry;
    final why = CutDivider.reason(cut.reason);
    final label = [
      'New session',
      if (why.isNotEmpty) why,
      if (carry != null) 'summary carried over',
    ].join(' · ');
    final line = Expanded(child: Container(height: 1, color: c.hairline));
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Row(
            children: [
              line,
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 10),
                child: InkWell(
                  borderRadius: BorderRadius.circular(6),
                  onTap: carry == null
                      ? null
                      : () => setState(
                          () => widget.item.expanded = !widget.item.expanded,
                        ),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 6,
                      vertical: 2,
                    ),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          label,
                          style: TextStyle(fontSize: 11.5, color: c.muted),
                        ),
                        if (carry != null) ...[
                          const SizedBox(width: 2),
                          Icon(
                            widget.item.expanded
                                ? Icons.expand_less
                                : Icons.expand_more,
                            size: 14,
                            color: c.muted,
                          ),
                        ],
                      ],
                    ),
                  ),
                ),
              ),
              line,
              if (widget.onMove case final move?)
                _SmallIcon(
                  icon: Icons.drive_file_move_outline,
                  tooltip: 'Move this topic to a channel',
                  onTap: move,
                ),
            ],
          ),
        ),
        if (carry != null && widget.item.expanded)
          Container(
            margin: const EdgeInsets.only(top: 4),
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              color: c.card,
              border: Border.all(color: c.hairline),
              borderRadius: BorderRadius.circular(10),
            ),
            child: SelectableText(
              carry,
              style: TextStyle(fontSize: 12.5, color: c.muted, height: 1.4),
            ),
          ),
      ],
    );
  }
}

/// A post from elsewhere in a stream or a timeline: who said it, when, and
/// what. Its thread opens in the right pane.
class PostCard extends StatelessWidget {
  const PostCard({super.key, required this.post, required this.store});

  final Post post;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final session = post.sessionId;
    final from = store.channels
        .where((ch) => ch.id == post.pointsToChannel)
        .firstOrNull;
    final tag = switch (post.authorKind) {
      'routine' => (Icons.schedule, 'routine'),
      'subagent' => (Icons.subdirectory_arrow_right, 'agent'),
      _ => null,
    };
    final elsewhere =
        post.kind == 'pointer' && (post.isRequest || post.outboxId != null);
    // A channel suggestion is decided with its own buttons, below.
    final suggestion = post.suggestion;
    return Container(
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(12),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              authorAvatar(
                store,
                authorId: post.authorId,
                name: post.author,
                size: 20,
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Row(
                  children: [
                    Flexible(
                      child: Text(
                        store.authorName(post.authorId, post.author),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          fontSize: 13,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                    if (post.tainted) ...[
                      const SizedBox(width: 5),
                      const TaintMark(),
                    ],
                    if (tag case (final icon, final words)) ...[
                      const SizedBox(width: 6),
                      Tooltip(
                        message: words,
                        child: Icon(icon, size: 13, color: c.muted),
                      ),
                    ],
                    const SizedBox(width: 8),
                    Text(
                      relativeTime(post.createdAt),
                      style: TextStyle(fontSize: 11.5, color: c.muted),
                    ),
                    if (post.needsYou) ...[
                      const SizedBox(width: 8),
                      NeedsYouTag(where: from == null ? null : '#${from.slug}'),
                    ],
                  ],
                ),
              ),
              // A pointer: go to what waits, or mark it done here. One to
              // a request or outbox item has its buttons under the text.
              if (elsewhere || suggestion != null)
                const SizedBox.shrink()
              else if (post.outboxId != null)
                // Decided in the outbox cards above the composer.
                _CardButton(
                  label: post.needsYou ? 'Waiting below' : 'Decided ✓',
                  onPressed: null,
                )
              else if (post.kind == 'pointer') ...[
                if (from != null)
                  _CardButton(
                    label: 'Go to ${store.conversationName(from.id)}',
                    onPressed: () => store.openConversation(from.id),
                  ),
                _CardButton(
                  label: post.needsYou ? 'Done' : 'Done ✓',
                  onPressed: post.needsYou
                      ? () => store.resolvePost(post)
                      : null,
                ),
              ] else if (post.kind == 'delegated' && session != null) ...[
                // Work done for another agent: its sub-runs live in the
                // caller's thread. Conversations are flat, so it's continued
                // by writing in this DM (the API's `reply_to` still carries
                // the brief and reports).
                if (post.text.startsWith('Working'))
                  Padding(
                    padding: const EdgeInsets.only(right: 6),
                    child: SizedBox.square(
                      dimension: 11,
                      child: CircularProgressIndicator(
                        strokeWidth: 1.5,
                        color: c.muted,
                      ),
                    ),
                  ),
                _CardButton(
                  label: 'Open',
                  onPressed: () => store.openThread(session),
                ),
              ] else if (session != null)
                TextButton(
                  onPressed: () => store.openThread(session),
                  style: TextButton.styleFrom(
                    visualDensity: VisualDensity.compact,
                    padding: const EdgeInsets.symmetric(horizontal: 8),
                  ),
                  child: Text(
                    'Open',
                    style: TextStyle(fontSize: 12, color: c.link),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 6),
          MarkdownBody(
            data: tidyMarkdown(post.text),
            selectable: true,
            onTapLink: (_, href, _) => openTernLink(context, store, href),
            styleSheet: MarkdownStyleSheet.fromTheme(Theme.of(context))
                .copyWith(
                  p: Theme.of(context).textTheme.bodyMedium,
                  a: TextStyle(
                    color: context.colors.link,
                    fontWeight: FontWeight.w600,
                    decoration: TextDecoration.underline,
                  ),
                ),
          ),
          for (final id in routineLinksIn(post.text))
            RoutineCard(item: RoutineItem(id), store: store),
          if (suggestion != null)
            SuggestionActions(suggestion: suggestion, store: store)
          else if (elsewhere) ...[
            const SizedBox(height: 4),
            Wrap(spacing: 4, children: _waitingElsewhere(context)),
          ],
        ],
      ),
    );
  }
}

extension on PostCard {
  /// A Home pointer to an approval, secret request or outbox item waiting
  /// in another conversation: open it there, or answer a simple one here.
  List<Widget> _waitingElsewhere(BuildContext context) {
    final to = post.pointsToChannel;
    final go = to == null
        ? null
        : _CardButton(
            label: 'Go to ${store.conversationName(to)}',
            onPressed: () => store.openConversation(to),
          );
    if (!post.needsYou) {
      return [const _CardButton(label: 'Decided ✓', onPressed: null)];
    }
    if (post.outboxId case final id?) {
      final item = store.outbox
          .where((o) => o.id == id && o.pending)
          .firstOrNull;
      return [
        ?go,
        if (item != null) ...[
          _CardButton(
            label: 'Approve',
            onPressed: () => store.decideOutbox(item, approve: true),
          ),
          _CardButton(
            label: 'Reject',
            onPressed: () => store.decideOutbox(item, approve: false),
          ),
        ],
      ];
    }
    // An approval can be allowed once or denied from here; "always" and
    // the host limit's choices, and secrets, are on the card where it
    // waits.
    final card = store.requestCard(post);
    return [
      ?go,
      if (card case final ApprovalItem a when a.outcome == null) ...[
        _CardButton(
          label: 'Approve once',
          onPressed: () => store.answer(a, Decision.once),
        ),
        _CardButton(
          label: 'Deny',
          onPressed: () => store.answer(a, Decision.deny),
        ),
      ],
    ];
  }
}

class _CardButton extends StatelessWidget {
  const _CardButton({required this.label, required this.onPressed});

  final String label;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) => TextButton(
    onPressed: onPressed,
    style: TextButton.styleFrom(
      visualDensity: VisualDensity.compact,
      padding: const EdgeInsets.symmetric(horizontal: 8),
    ),
    child: Text(
      label,
      style: TextStyle(
        fontSize: 12,
        color: onPressed == null ? context.colors.muted : context.colors.accent,
      ),
    ),
  );
}

class _SmallIcon extends StatelessWidget {
  const _SmallIcon({
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Tooltip(
    message: tooltip,
    child: InkWell(
      borderRadius: BorderRadius.circular(6),
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.all(4),
        child: Icon(icon, size: 14, color: context.colors.muted),
      ),
    ),
  );
}

/// Where a conversation was before it moved to a channel: a quiet line
/// that goes there.
class MovedMarker extends StatelessWidget {
  const MovedMarker({super.key, required this.post, required this.store});

  final Post post;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final to = post.pointsToChannel;
    return Center(
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: to == null ? null : () => store.openChannelView(to),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.drive_file_move_outline, size: 14, color: c.muted),
              const SizedBox(width: 6),
              Flexible(
                child: Text(
                  post.text,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              ),
              if (post.tainted) ...[
                const SizedBox(width: 4),
                Tooltip(
                  message: 'Read untrusted content',
                  child: Icon(Icons.shield_outlined, size: 12, color: c.muted),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// A `channel_suggest` card's choices: create the channel (moving this
/// conversation and the suggested routines there, each can be left out)
/// or not now. Decided, it says how.
class SuggestionActions extends StatefulWidget {
  const SuggestionActions({
    super.key,
    required this.suggestion,
    required this.store,
  });

  final ChannelSuggestion suggestion;
  final TernStore store;

  @override
  State<SuggestionActions> createState() => _SuggestionActionsState();
}

class _SuggestionActionsState extends State<SuggestionActions> {
  late var _move = widget.suggestion.sessionId != null;
  late final _routines = {for (final r in widget.suggestion.routines) r.id};
  var _busy = false;
  String? _error;

  Future<void> _decide(bool accept) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final s = widget.suggestion;
    final err = accept
        ? await widget.store.acceptSuggestion(
            s,
            moveConversation: _move,
            routines: _routines.toList(),
          )
        : await widget.store.declineSuggestion(s);
    if (!mounted) return;
    setState(() {
      _busy = false;
      _error = err;
    });
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = widget.suggestion;
    if (!s.pending) {
      return Padding(
        padding: const EdgeInsets.only(top: 4),
        child: s.channelId == null
            ? const _CardButton(label: 'Declined', onPressed: null)
            : _CardButton(
                label: 'Go to #${s.slug}',
                onPressed: () => widget.store.openChannelView(s.channelId!),
              ),
      );
    }
    Widget check(String label, bool value, ValueChanged<bool> on) => InkWell(
      borderRadius: BorderRadius.circular(6),
      onTap: _busy ? null : () => on(!value),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Checkbox(
            value: value,
            visualDensity: VisualDensity.compact,
            onChanged: _busy ? null : (v) => on(v ?? false),
          ),
          Flexible(child: Text(label, style: const TextStyle(fontSize: 12.5))),
        ],
      ),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SizedBox(height: 2),
        if (s.sessionId != null)
          check(
            'Move this conversation there',
            _move,
            (v) => setState(() => _move = v),
          ),
        for (final r in s.routines)
          check(
            'Move the routine "${r.title}" there',
            _routines.contains(r.id),
            (v) => setState(
              () => v ? _routines.add(r.id) : _routines.remove(r.id),
            ),
          ),
        if (_error case final e?)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Text(e, style: TextStyle(color: c.error, fontSize: 12)),
          ),
        const SizedBox(height: 4),
        Wrap(
          spacing: 6,
          children: [
            FilledButton(
              onPressed: _busy ? null : () => _decide(true),
              style: FilledButton.styleFrom(
                visualDensity: VisualDensity.compact,
              ),
              child: Text('Create #${s.slug}'),
            ),
            TextButton(
              onPressed: _busy ? null : () => _decide(false),
              child: const Text('Not now'),
            ),
          ],
        ),
      ],
    );
  }
}
