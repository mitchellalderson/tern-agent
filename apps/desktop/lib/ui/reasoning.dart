// The model's reasoning, shown quietly: a chevron beside "Thinking" while
// it streams, and a "Thought for 12s" note on the message after.

import 'package:flutter/material.dart';

import '../state/transcript.dart';
import '../theme.dart';
import 'items.dart' show formatDuration;

/// Shown in place of text when the provider withheld the reasoning.
const reasoningHiddenText = 'Reasoning hidden by provider';

/// "Thought for 12s", or just "Thought" under a second.
String thoughtLabel(Reasoning r) {
  final d = r.duration();
  return d.inSeconds >= 1 ? 'Thought for ${formatDuration(d)}' : 'Thought';
}

/// A small chevron that says the line above it opens.
class ReasoningChevron extends StatelessWidget {
  const ReasoningChevron({super.key, required this.open});
  final bool open;

  @override
  Widget build(BuildContext context) => Icon(
    open ? Icons.expand_less : Icons.expand_more,
    key: const ValueKey('reasoning-chevron'),
    size: 16,
    color: context.colors.muted,
  );
}

/// The reasoning text in a quiet surface: muted, one step off the page,
/// no border, a capped height with its own scroll. It follows the end
/// while text streams in, unless the reader scrolled up.
class ReasoningBox extends StatefulWidget {
  const ReasoningBox({super.key, required this.reasoning});
  final Reasoning reasoning;

  /// The most it grows before it scrolls.
  static const maxHeight = 200.0;

  @override
  State<ReasoningBox> createState() => _ReasoningBoxState();
}

class _ReasoningBoxState extends State<ReasoningBox> {
  final _scroll = ScrollController();
  var _follow = true;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(() {
      if (!_scroll.hasClients) return;
      final p = _scroll.position;
      // Within a few pixels of the end counts as "at the bottom".
      _follow = p.maxScrollExtent - p.pixels < 24;
    });
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final r = widget.reasoning;
    final text = r.text.isEmpty && r.hidden ? reasoningHiddenText : r.text;
    if (_follow) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (_follow && _scroll.hasClients) {
          _scroll.jumpTo(_scroll.position.maxScrollExtent);
        }
      });
    }
    return Container(
      key: const ValueKey('reasoning-box'),
      width: double.infinity,
      constraints: const BoxConstraints(maxHeight: ReasoningBox.maxHeight),
      decoration: BoxDecoration(
        color: c.sidebar,
        borderRadius: BorderRadius.circular(6),
      ),
      child: Scrollbar(
        controller: _scroll,
        child: SingleChildScrollView(
          controller: _scroll,
          padding: const EdgeInsets.all(16),
          child: SelectableText(
            text,
            style: TextStyle(fontSize: 12.5, height: 1.5, color: c.muted),
          ),
        ),
      ),
    );
  }
}

/// "Thought for 12s ›": the kept reasoning of a message, closed until
/// the reader opens it.
class ReasoningNote extends StatefulWidget {
  const ReasoningNote({super.key, required this.reasoning});
  final Reasoning reasoning;

  @override
  State<ReasoningNote> createState() => _ReasoningNoteState();
}

class _ReasoningNoteState extends State<ReasoningNote> {
  var _open = false;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          key: const ValueKey('reasoning-toggle'),
          onTap: () => setState(() => _open = !_open),
          borderRadius: BorderRadius.circular(6),
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 2),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  thoughtLabel(widget.reasoning),
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
                const SizedBox(width: 2),
                ReasoningChevron(open: _open),
              ],
            ),
          ),
        ),
        if (_open) ...[
          const SizedBox(height: 8),
          ReasoningBox(reasoning: widget.reasoning),
          const SizedBox(height: 4),
        ],
      ],
    );
  }
}
