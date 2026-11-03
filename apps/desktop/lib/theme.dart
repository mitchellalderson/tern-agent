import 'package:flutter/material.dart';

/// Design tokens (docs/gui-spec.md §8). Light by default; dark is its own
/// set, not an inversion.
@immutable
class TernColors extends ThemeExtension<TernColors> {
  const TernColors({
    required this.background,
    required this.sidebar,
    required this.card,
    required this.hairline,
    required this.text,
    required this.muted,
    required this.accent,
    required this.onAccent,
    required this.ok,
    required this.warn,
    required this.error,
    required this.userBubble,
    required this.selection,
  });

  final Color background;
  final Color sidebar;
  final Color card;
  final Color hairline;
  final Color text;
  final Color muted;
  final Color accent;
  final Color onAccent;
  final Color ok;
  final Color warn;
  final Color error;
  final Color userBubble;
  final Color selection;

  static const light = TernColors(
    background: Color(0xFFFFFFFF),
    sidebar: Color(0xFFF6F6F4),
    card: Color(0xFFF7F7F5),
    hairline: Color(0xFFE7E7E3),
    text: Color(0xFF1C1C1A),
    muted: Color(0xFF6B6B66),
    accent: Color(0xFFF07A2B),
    onAccent: Color(0xFFFFFFFF),
    ok: Color(0xFF2F9E5B),
    warn: Color(0xFFD9902B),
    error: Color(0xFFD2453B),
    userBubble: Color(0xFFF0F0ED),
    selection: Color(0xFFEAEAE6),
  );

  static const dark = TernColors(
    background: Color(0xFF161615),
    sidebar: Color(0xFF1E1E1C),
    card: Color(0xFF222220),
    hairline: Color(0xFF30302D),
    text: Color(0xFFEDEDEA),
    muted: Color(0xFF9A9A94),
    accent: Color(0xFFF58A42),
    onAccent: Color(0xFF1C1C1A),
    ok: Color(0xFF4CBB78),
    warn: Color(0xFFE6A54A),
    error: Color(0xFFE5685F),
    userBubble: Color(0xFF2A2A27),
    selection: Color(0xFF2C2C29),
  );

  @override
  TernColors copyWith() => this;

  @override
  TernColors lerp(TernColors? other, double t) =>
      t < 0.5 || other == null ? this : other;
}

extension TernTheme on BuildContext {
  TernColors get colors => Theme.of(this).extension<TernColors>()!;
}

ThemeData ternTheme(TernColors c, Brightness b) {
  final base = ThemeData(
    brightness: b,
    useMaterial3: true,
    colorScheme: ColorScheme.fromSeed(
      seedColor: c.accent,
      brightness: b,
      primary: c.accent,
      onPrimary: c.onAccent,
      surface: c.background,
      onSurface: c.text,
      error: c.error,
    ),
    scaffoldBackgroundColor: c.background,
    dividerColor: c.hairline,
    splashFactory: NoSplash.splashFactory,
    highlightColor: Colors.transparent,
    hoverColor: c.selection.withValues(alpha: 0.6),
    visualDensity: VisualDensity.compact,
    extensions: [c],
  );
  return base.copyWith(
    textTheme: base.textTheme
        .apply(bodyColor: c.text, displayColor: c.text)
        .copyWith(
          bodyMedium: base.textTheme.bodyMedium?.copyWith(
            fontSize: 14,
            height: 1.45,
            color: c.text,
          ),
          bodySmall: base.textTheme.bodySmall?.copyWith(
            fontSize: 12.5,
            color: c.muted,
          ),
          titleSmall: base.textTheme.titleSmall?.copyWith(
            fontSize: 13.5,
            fontWeight: FontWeight.w600,
            color: c.text,
          ),
          labelSmall: base.textTheme.labelSmall?.copyWith(
            fontSize: 11,
            letterSpacing: 0.6,
            fontWeight: FontWeight.w600,
            color: c.muted,
          ),
        ),
    tooltipTheme: TooltipThemeData(
      waitDuration: const Duration(milliseconds: 600),
      textStyle: TextStyle(fontSize: 12, color: c.background),
      decoration: BoxDecoration(
        color: c.text,
        borderRadius: BorderRadius.circular(6),
      ),
    ),
  );
}

const mono = TextStyle(
  fontFamily: 'Menlo',
  fontFamilyFallback: ['SF Mono', 'DejaVu Sans Mono', 'monospace'],
  fontSize: 12,
  height: 1.4,
);
