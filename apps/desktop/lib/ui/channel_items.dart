import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import 'agent_face.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'channel_view.dart' show NeedsYouTag;
import 'move_dialog.dart';
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
              // A pointer: go to what waits, or mark it done here.
              if (post.outboxId != null)
                // Decided in the outbox cards above the composer.
                _CardButton(
                  label: post.needsYou ? 'Waiting below' : 'Decided ✓',
                  onPressed: null,
                )
              else if (post.kind == 'pointer') ...[
                if (from != null)
                  _CardButton(
                    label: 'Go to #${from.slug}',
                    onPressed: () => store.openChannelView(from.id),
                  ),
                _CardButton(
                  label: post.needsYou ? 'Done' : 'Done ✓',
                  onPressed: post.needsYou
                      ? () => store.resolvePost(post)
                      : null,
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
                    style: TextStyle(fontSize: 12, color: c.accent),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 6),
          MarkdownBody(
            data: post.text,
            selectable: true,
            styleSheet: MarkdownStyleSheet.fromTheme(Theme.of(context))
                .copyWith(p: Theme.of(context).textTheme.bodyMedium),
          ),
        ],
      ),
    );
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

