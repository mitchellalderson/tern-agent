import 'dart:io';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme.dart';

const _window = MethodChannel('tern/window');

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
  const AgentAvatar({super.key, required this.name, this.size = 28});

  final String name;
  final double size;

  static const _palette = [
    Color(0xFFF59E3B),
    Color(0xFF3FB6A8),
    Color(0xFF7C6CF2),
    Color(0xFFE0607E),
    Color(0xFF4C8DF6),
    Color(0xFFF06A3C),
    Color(0xFF9B6AE8),
    Color(0xFF5BAE5B),
  ];

  @override
  Widget build(BuildContext context) {
    final color =
        _palette[name.codeUnits.fold(0, (a, b) => a + b) % _palette.length];
    final eye = Container(
      width: size * 0.1,
      height: size * 0.26,
      decoration: BoxDecoration(
        color: Colors.white,
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

String formatTokens(int n) =>
    n >= 1000 ? '${(n / 1000).toStringAsFixed(n >= 10000 ? 0 : 1)}k' : '$n';
