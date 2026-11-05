import 'dart:async';
import 'dart:io';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';
import 'package:flutter/services.dart';

import '../theme.dart';

const _window = MethodChannel('tern/window');

/// A chat gateway's name and icon for a session that came from it
/// (Telegram, or a gateway connector such as ntfy or Slack); null for
/// tern's own sessions.
({String label, IconData icon})? gatewaySource(String source) =>
    switch (source) {
      'telegram' => (label: 'Telegram', icon: Icons.send_outlined),
      'ntfy' => (label: 'ntfy', icon: Icons.notifications_outlined),
      'slack' => (label: 'Slack', icon: Icons.tag),
      _ => null,
    };

/// Open `url` in the default browser.
Future<void> openUrl(String url) async {
  final opener = Platform.isMacOS ? 'open' : 'xdg-open';
  try {
    await Process.run(opener, [url]);
  } on ProcessException {
    // No browser launcher; the link text is still visible.
  }
}

/// Space the macOS traffic lights need above the sidebar.
double get titleBarInset => Platform.isMacOS ? 40 : 8;

/// A header region that drags the window (and zooms on double-click), as the
/// title bar would. Only needed on macOS, where content fills the title bar.
class WindowDragArea extends StatelessWidget {
  const WindowDragArea({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (!Platform.isMacOS) return child;
    return RawGestureDetector(
      behavior: HitTestBehavior.translucent,
      gestures: {
        PanGestureRecognizer:
            GestureRecognizerFactoryWithHandlers<PanGestureRecognizer>(
              PanGestureRecognizer.new,
              (r) => r.onStart = (_) => _window.invokeMethod<void>('drag'),
            ),
        DoubleTapGestureRecognizer:
            GestureRecognizerFactoryWithHandlers<DoubleTapGestureRecognizer>(
              DoubleTapGestureRecognizer.new,
              (r) => r.onDoubleTap = () => _window.invokeMethod<void>('zoom'),
            ),
      },
      child: child,
    );
  }
}

/// The agent's face: a rounded shape with two eyes, coloured by name.
class AgentAvatar extends StatelessWidget {
  const AgentAvatar({
    super.key,
    required this.name,
    this.size = 28,
    this.glyph,
    this.color,
  });

  /// An agent profile's avatar: its glyph and colour, or generated from
  /// its id (gui-spec §8).
  factory AgentAvatar.of(Agent a, {double size = 28, Key? key}) => AgentAvatar(
    key: key,
    name: a.id,
    size: size,
    glyph: a.avatar?.glyph,
    color: a.avatar?.color,
  );

  /// Picks the colour when [color] is unset.
  final String name;
  final double size;

  /// Shown instead of the eyes.
  final String? glyph;

  /// One of [colorNames].
  final String? color;

  /// The eight avatar colours, by the names the API uses.
  static const colorNames = [
    'amber',
    'teal',
    'violet',
    'pink',
    'blue',
    'orange',
    'purple',
    'green',
  ];

  /// Glyphs offered by the agent sheet.
  static const glyphs = ['●', '■', '▲', '★', '♥', '♦', '♣', '♠', '☾', '✓'];

  /// Stripe, status and earth tones only (no red, no pastels).
  static const _palette = [
    Color(0xFFF6B92B),
    Color(0xFF8DB56F),
    Color(0xFFB89A78),
    Color(0xFFF5A55F),
    Color(0xFF7FA6A0),
    Color(0xFFF07A2B),
    Color(0xFF6F604F),
    Color(0xFF6F8F4E),
  ];

  static Color colorFor(String name, [String? color]) {
    final i = color == null ? -1 : colorNames.indexOf(color);
    return _palette[i >= 0
        ? i
        : name.codeUnits.fold(0, (a, b) => a + b) % _palette.length];
  }

  @override
  Widget build(BuildContext context) {
    final color = colorFor(name, this.color);
    if (glyph case final g?) {
      return Container(
        width: size,
        height: size,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: color,
          borderRadius: BorderRadius.circular(size * 0.42),
        ),
        child: Text(
          g,
          style: TextStyle(
            color: const Color(0xFF2B2118),
            fontSize: size * 0.5,
            height: 1,
            fontWeight: FontWeight.w600,
          ),
        ),
      );
    }
    final eye = Container(
      width: size * 0.1,
      height: size * 0.26,
      decoration: BoxDecoration(
        color: const Color(0xFF2B2118),
        borderRadius: BorderRadius.circular(size),
      ),
    );
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        color: color,
        borderRadius: BorderRadius.circular(size * 0.42),
      ),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          eye,
          SizedBox(width: size * 0.14),
          eye,
        ],
      ),
    );
  }
}

/// A [QuietIconButton] that copies [text] to the clipboard and shows a check
/// for a moment. [text] is read on click, so it can describe live state.
class CopyButton extends StatefulWidget {
  const CopyButton({
    super.key,
    required this.text,
    this.tooltip = 'Copy',
    this.size = 15,
  });

  final String Function() text;
  final String tooltip;
  final double size;

  @override
  State<CopyButton> createState() => _CopyButtonState();
}

class _CopyButtonState extends State<CopyButton> {
  Timer? _reset;

  @override
  void dispose() {
    _reset?.cancel();
    super.dispose();
  }

  Future<void> _copy() async {
    await Clipboard.setData(ClipboardData(text: widget.text()));
    if (!mounted) return;
    _reset?.cancel();
    setState(
      () => _reset = Timer(const Duration(milliseconds: 1500), () {
        if (mounted) setState(() => _reset = null);
      }),
    );
  }

  @override
  Widget build(BuildContext context) => QuietIconButton(
    icon: _reset == null ? Icons.copy_rounded : Icons.check_rounded,
    tooltip: _reset == null ? widget.tooltip : 'Copied',
    size: widget.size,
    onPressed: _copy,
  );
}

/// A small borderless icon button with a tooltip.
class QuietIconButton extends StatelessWidget {
  const QuietIconButton({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.onPressed,
    this.size = 18,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;
  final double size;

  @override
  Widget build(BuildContext context) => IconButton(
    icon: Icon(icon, size: size),
    tooltip: tooltip,
    onPressed: onPressed,
    color: context.colors.muted,
    splashRadius: 16,
    visualDensity: VisualDensity.compact,
    style: IconButton.styleFrom(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
    ),
  );
}

class Hairline extends StatelessWidget {
  const Hairline({super.key, this.vertical = false});

  final bool vertical;

  @override
  Widget build(BuildContext context) => Container(
    width: vertical ? 1 : null,
    height: vertical ? null : 1,
    color: context.colors.hairline,
  );
}

/// The subtle marker on a post written by a tainted run.
class TaintMark extends StatelessWidget {
  const TaintMark({super.key});

  @override
  Widget build(BuildContext context) => Tooltip(
    message: 'Written by a run that read untrusted content (mail, a web page…)',
    child: Icon(
      Icons.shield_outlined,
      size: 13,
      color: context.colors.warn.withValues(alpha: 0.8),
    ),
  );
}

/// A small amber "untrusted" mark for a tainted run or post: it read
/// third-party text, so its writes ask. [reason] becomes the tooltip.
class TaintBadge extends StatelessWidget {
  const TaintBadge({super.key, this.reason, this.label = 'untrusted'});

  final String? reason;
  final String label;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Tooltip(
      message: reason == null
          ? 'Written by a run that read untrusted content'
          : 'This run read untrusted content ($reason), so its changes ask first',
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.shield_outlined, size: 11, color: c.warn),
          const SizedBox(width: 3),
          Text(label, style: TextStyle(fontSize: 11, color: c.warn)),
        ],
      ),
    );
  }
}

String relativeTime(DateTime t, [DateTime? now]) {
  now ??= DateTime.now();
  final d = now.difference(t);
  if (d.inMinutes < 1) return 'now';
  if (d.inHours < 1) return '${d.inMinutes}m';
  if (now.day == t.day && d.inHours < 24) {
    final h = t.hour % 12 == 0 ? 12 : t.hour % 12;
    return '$h:${t.minute.toString().padLeft(2, '0')} ${t.hour < 12 ? 'AM' : 'PM'}';
  }
  if (d.inDays < 2) return 'Yesterday';
  if (d.inDays < 7) {
    return const ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'][t.weekday -
        1];
  }
  return '${t.month}/${t.day}';
}

String formatTokens(int n) => n >= 1000000
    ? '${(n / 1000000).toStringAsFixed(n % 1000000 == 0 ? 0 : 1)}M'
    : n >= 1000
    ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k'
    : '$n';

/// How full a conversation's context is: a small ring and a percentage.
/// Turns amber near [compactAt], where the agent summarizes older messages.
class ContextMeter extends StatelessWidget {
  const ContextMeter({
    super.key,
    required this.used,
    required this.window,
    this.compactAt,
  });

  final int used;
  final int window;
  final int? compactAt;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final fraction = (used / window).clamp(0.0, 1.0);
    final near = compactAt != null && used >= compactAt! * 0.85;
    final color = fraction >= 0.95
        ? c.error
        : near
        ? c.warn
        : c.muted;
    final pct = (fraction * 100).round();
    return Tooltip(
      message: [
        'Context: ${formatTokens(used)} of ${formatTokens(window)} tokens',
        if (compactAt case final at?)
          'Older messages are summarized at ${formatTokens(at)}',
      ].join('\n'),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 6),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox.square(
              dimension: 14,
              child: CircularProgressIndicator(
                value: fraction,
                strokeWidth: 2.2,
                color: color,
                backgroundColor: c.hairline,
              ),
            ),
            const SizedBox(width: 6),
            Text(
              '${pct == 0 && used > 0 ? '<1' : pct}%',
              style: TextStyle(
                fontSize: 12.5,
                color: color,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A [Flexible] in a window of its own; when [embedded] in a scrolling page
/// (Settings), its child just takes the room it needs.
class MaybeFlexible extends StatelessWidget {
  const MaybeFlexible({super.key, required this.embedded, required this.child});

  final bool embedded;
  final Widget child;

  @override
  Widget build(BuildContext context) =>
      embedded ? child : Flexible(child: child);
}
