import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';

import '../theme.dart';
import 'feed.dart';
import 'markdown_text.dart';

/// A message longer than this many lines or characters shows its first
/// [previewLines] lines, and the rest on "Show more".
const collapseLines = 24;
const collapseChars = 2400;
const previewLines = 12;

/// The part of [text] to show while it is collapsed, or null when it is
/// short enough to show whole. Cut at a line end; a code fence left open is
/// closed, so the preview renders as Markdown.
({String text, int hiddenLines})? collapsedPreview(String text) {
  final lines = text.split('\n');
  if (lines.length <= collapseLines && text.length <= collapseChars) {
    return null;
  }
  final kept = <String>[];
  var chars = 0;
  for (final l in lines) {
    if (kept.length >= previewLines || chars > collapseChars ~/ 2) break;
    kept.add(l);
    chars += l.length + 1;
  }
  if (kept.length == lines.length) {
    // A few enormous lines: cut the last one.
    kept[kept.length - 1] = kept.last.substring(0, collapseChars ~/ 2);
  }
  final preview = closeOpenSpans(kept.join('\n'));
  return (text: preview, hiddenLines: lines.length - kept.length);
}

/// [builder] draws [text] whole, or, when it is long and [collapsible],
/// only its start with a "Show more" under it (and "Show less" after).
class Collapsible extends StatefulWidget {
  const Collapsible({
    super.key,
    required this.text,
    required this.builder,
    this.collapsible = true,
  });

  final String text;
  final Widget Function(String text) builder;

  /// False while it still streams: a growing message is never cut.
  final bool collapsible;

  @override
  State<Collapsible> createState() => _CollapsibleState();
}

class _CollapsibleState extends State<Collapsible> {
  var _open = false;

  /// Flip open/closed without moving what the reader is looking at. In a
  /// reversed feed a taller item grows upward from its bottom edge, so the
  /// text clicked would slide away; shift the scroll by the height change to
  /// keep the item's top where it was (and in view, after a collapse).
  void _toggle() {
    final before = context.size?.height;
    feedSkipNextGrowth(context);
    setState(() => _open = !_open);
    final scrollable = Scrollable.maybeOf(context);
    if (before == null || scrollable == null) return;
    if (scrollable.axisDirection != AxisDirection.up) return;
    SchedulerBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      final after = context.size?.height;
      final pos = scrollable.position;
      if (after == null || !pos.hasPixels) return;
      final target = (pos.pixels + after - before).clamp(
        pos.minScrollExtent,
        pos.maxScrollExtent,
      );
      pos.jumpTo(target);
      if (after < before) {
        // The top may now sit above the viewport: bring it back.
        final box = context.findRenderObject() as RenderBox?;
        final view = scrollable.context.findRenderObject() as RenderBox?;
        if (box == null || view == null || !box.attached) return;
        final top = box.localToGlobal(Offset.zero, ancestor: view).dy;
        if (top < 0) pos.jumpTo(pos.pixels + top);
      }
    }, debugLabel: 'collapsible-keep-top');
  }

  @override
  Widget build(BuildContext context) {
    final preview = widget.collapsible ? collapsedPreview(widget.text) : null;
    if (preview == null) return widget.builder(widget.text);
    final c = context.colors;
    final label = _open ? 'Show less' : 'Show more';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        widget.builder(_open ? widget.text : preview.text),
        Semantics(
          button: true,
          label: _open
              ? 'Show less of this message'
              : 'Show more of this message',
          excludeSemantics: true,
          child: InkWell(
            borderRadius: BorderRadius.circular(6),
            onTap: _toggle,
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 4, horizontal: 2),
              child: Text(
                _open || preview.hiddenLines == 0
                    ? label
                    : '$label (${preview.hiddenLines} more lines)',
                style: TextStyle(
                  fontSize: 12.5,
                  fontWeight: FontWeight.w600,
                  color: c.link,
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}
