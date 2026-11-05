import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';

import '../theme.dart';

/// One label in an [UnderlineTabs] row: plain text, a thin accent underline
/// when selected, muted otherwise. No border, fill or check mark.
class UnderlineTab extends StatefulWidget {
  const UnderlineTab({
    super.key,
    required this.label,
    required this.selected,
    required this.onTap,
    this.leading,
    this.fontSize = 13.5,
    this.exclusive = true,
  });

  final String label;
  final bool selected;
  final VoidCallback? onTap;
  final Widget? leading;
  final double fontSize;

  /// False for toggles that can be on together (an agent picker).
  final bool exclusive;

  @override
  State<UnderlineTab> createState() => _UnderlineTabState();
}

class _UnderlineTabState extends State<UnderlineTab> {
  bool _hover = false;
  bool _focus = false;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final still = MediaQuery.disableAnimationsOf(context);
    final on = widget.selected;
    final color = on
        ? c.text
        : (_hover || _focus)
        ? c.text.withValues(alpha: 0.85)
        : c.muted;
    return Semantics(
      container: true,
      button: true,
      selected: on,
      inMutuallyExclusiveGroup: widget.exclusive,
      role: widget.exclusive ? SemanticsRole.tab : SemanticsRole.none,
      label: widget.label,
      excludeSemantics: true,
      onTap: widget.onTap,
      child: InkWell(
        onTap: widget.onTap,
        onHover: (v) => setState(() => _hover = v),
        onFocusChange: (v) => setState(() => _focus = v),
        splashFactory: NoSplash.splashFactory,
        highlightColor: Colors.transparent,
        hoverColor: Colors.transparent,
        focusColor: Colors.transparent,
        borderRadius: BorderRadius.circular(4),
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 44, minWidth: 44),
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 4),
            foregroundDecoration: _focus
                ? BoxDecoration(
                    border: Border.all(color: c.accent, width: 2),
                    borderRadius: BorderRadius.circular(4),
                  )
                : null,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                const SizedBox(height: 10),
                Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (widget.leading != null) ...[
                      widget.leading!,
                      const SizedBox(width: 6),
                    ],
                    Text(
                      widget.label,
                      style: TextStyle(
                        fontSize: widget.fontSize,
                        color: color,
                        fontWeight: on ? FontWeight.w600 : FontWeight.w400,
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 6),
                AnimatedContainer(
                  duration: still
                      ? Duration.zero
                      : const Duration(milliseconds: 150),
                  height: 2,
                  color: on
                      ? c.accent
                      : (_hover ? c.hairline : Colors.transparent),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// A flat row of [UnderlineTab]s that wraps when narrow. The one control for
/// filters and view switches.
class UnderlineTabs extends StatelessWidget {
  const UnderlineTabs({super.key, required this.tabs, this.spacing = 16});

  final List<UnderlineTab> tabs;
  final double spacing;

  @override
  Widget build(BuildContext context) =>
      Wrap(spacing: spacing, runSpacing: 0, children: tabs);
}
