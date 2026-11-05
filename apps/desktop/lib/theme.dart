import 'package:flutter/material.dart';

/// Design tokens (docs/brand/brand-guide.md). Cream paper, ink, and the
/// orange / yellow / teal racing stripe. Light by default; dark is its own
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
    required this.ink,
    required this.link,
    required this.stripeOrange,
    required this.stripeYellow,
    required this.stripeTeal,
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

  /// Interactive borders and strong text; `text` is the same colour.
  final Color ink;
  final Color link;
  final Color stripeOrange;
  final Color stripeYellow;
  final Color stripeTeal;

  static const light = TernColors(
    background: Color(0xFFF7EEDC),
    sidebar: Color(0xFFEFE3CB),
    card: Color(0xFFFCF7EB),
    hairline: Color(0xFFE0D2B6),
    text: Color(0xFF2B2118),
    muted: Color(0xFF6F604F),
    accent: Color(0xFFF07A2B),
    onAccent: Color(0xFF2B2118),
    ok: Color(0xFF4F7A3A),
    warn: Color(0xFFB8841A),
    error: Color(0xFFB3281D),
    userBubble: Color(0xFFE9DBBF),
    selection: Color(0xFFEFE3CB),
    ink: Color(0xFF2B2118),
    link: Color(0xFFB3281D),
    stripeOrange: Color(0xFFF07A2B),
    stripeYellow: Color(0xFFF6B92B),
    stripeTeal: Color(0xFF2A8C8C),
  );

  static const dark = TernColors(
    background: Color(0xFF1F1812),
    sidebar: Color(0xFF261E17),
    card: Color(0xFF2A211A),
    hairline: Color(0xFF3D3226),
    text: Color(0xFFF3E8D2),
    muted: Color(0xFFB3A58F),
    accent: Color(0xFFF58A3D),
    onAccent: Color(0xFF2B2118),
    ok: Color(0xFF8DB56F),
    warn: Color(0xFFE3B04B),
    error: Color(0xFFF08A7A),
    userBubble: Color(0xFF33281E),
    selection: Color(0xFF2F251C),
    ink: Color(0xFFF3E8D2),
    link: Color(0xFFF08A7A),
    stripeOrange: Color(0xFFF58A3D),
    stripeYellow: Color(0xFFF9C846),
    stripeTeal: Color(0xFF4DB3AE),
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
    fontFamily: 'TernSans',
    splashFactory: NoSplash.splashFactory,
    highlightColor: Colors.transparent,
    hoverColor: c.selection.withValues(alpha: 0.7),
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
          titleLarge: base.textTheme.titleLarge?.copyWith(
            fontFamily: 'TernDisplay',
            fontStyle: FontStyle.italic,
            fontWeight: FontWeight.w800,
            fontSize: 22,
            color: c.text,
          ),
          titleSmall: base.textTheme.titleSmall?.copyWith(
            fontSize: 13.5,
            fontWeight: FontWeight.w700,
            color: c.text,
          ),
          labelSmall: base.textTheme.labelSmall?.copyWith(
            fontFamily: 'TernSans',
            fontSize: 12,
            fontWeight: FontWeight.w600,
            color: c.muted,
          ),
        ),
    tooltipTheme: TooltipThemeData(
      waitDuration: const Duration(milliseconds: 600),
      textStyle: TextStyle(
        fontFamily: 'TernSans',
        fontSize: 12,
        color: c.background,
      ),
      decoration: BoxDecoration(
        color: c.text,
        borderRadius: BorderRadius.circular(8),
      ),
    ),
    dividerTheme: DividerThemeData(color: c.hairline, thickness: 1, space: 1),
    inputDecorationTheme: InputDecorationTheme(
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: c.hairline, width: 1.5),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style:
          FilledButton.styleFrom(
            backgroundColor: c.accent,
            foregroundColor: c.onAccent,
            textStyle: const TextStyle(
              fontFamily: 'TernSans',
              fontWeight: FontWeight.w700,
            ),
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(8),
              side: BorderSide(color: c.ink, width: 2),
            ),
          ).copyWith(
            backgroundColor: WidgetStateProperty.resolveWith(
              (s) =>
                  s.contains(WidgetState.hovered) ? c.stripeYellow : c.accent,
            ),
          ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        foregroundColor: c.ink,
        textStyle: const TextStyle(
          fontFamily: 'TernSans',
          fontWeight: FontWeight.w700,
        ),
        side: BorderSide(color: c.ink, width: 2),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        foregroundColor: c.link,
        textStyle: const TextStyle(
          fontFamily: 'TernSans',
          fontWeight: FontWeight.w600,
        ),
      ),
    ),
    cardTheme: CardThemeData(
      color: c.card,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: c.hairline),
      ),
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: c.card,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: c.ink, width: 1.5),
      ),
    ),
  );
}

const mono = TextStyle(
  fontFamily: 'TernMono',
  fontFamilyFallback: ['Menlo', 'SF Mono', 'DejaVu Sans Mono', 'monospace'],
  fontSize: 12,
  height: 1.4,
);

/// The tern wordmark: lowercase, display italic, tight tracking.
TextStyle wordmarkStyle(double size, Color color) => TextStyle(
  fontFamily: 'TernDisplay',
  fontStyle: FontStyle.italic,
  fontWeight: FontWeight.w900,
  fontSize: size,
  height: 1,
  letterSpacing: -size * 0.03,
  color: color,
);

/// The racing stripe: three hard-edged bars, orange / yellow / teal, never
/// reordered. [slant] skews it -18° for the Baja look.
class Stripes extends StatelessWidget {
  const Stripes({
    super.key,
    this.thickness = 4,
    this.width,
    this.vertical = false,
    this.slant = false,
  });

  final double thickness;
  final double? width;
  final bool vertical;
  final bool slant;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final bars = [
      c.stripeOrange,
      c.stripeYellow,
      c.stripeTeal,
    ].map((color) => Expanded(child: ColoredBox(color: color))).toList();
    final box = vertical
        ? SizedBox(
            width: thickness * 3,
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: bars,
            ),
          )
        : SizedBox(
            width: width,
            height: thickness * 3,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: bars,
            ),
          );
    return slant
        ? Transform(
            transform: Matrix4.skewX(-0.3249),
            alignment: Alignment.centerLeft,
            child: box,
          )
        : box;
  }
}

/// Shape for selected rows in lists: the usual 8px rounded rect, plus the
/// 3px orange edge on the left when [selected].
ShapeBorder rowShape(BuildContext context, {required bool selected}) => selected
    ? _EdgeShape(context.colors.stripeOrange)
    : RoundedRectangleBorder(borderRadius: BorderRadius.circular(8));

class _EdgeShape extends RoundedRectangleBorder {
  _EdgeShape(this.bar) : super(borderRadius: BorderRadius.circular(8));

  final Color bar;

  @override
  void paint(Canvas canvas, Rect rect, {TextDirection? textDirection}) {
    canvas.drawRRect(
      RRect.fromRectAndCorners(
        Rect.fromLTWH(rect.left, rect.top + 5, 3, rect.height - 10),
        topRight: const Radius.circular(2),
        bottomRight: const Radius.circular(2),
      ),
      Paint()..color = bar,
    );
  }
}
