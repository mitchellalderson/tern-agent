import 'package:flutter/material.dart';

import '../state/fleet.dart';
import '../state/store.dart';
import '../theme.dart';

/// A monotone mark for an [AgentStatus]. The shape differs per state, so it
/// never relies on colour alone: filled circle working (it pulses, unless
/// the system asks for reduced motion), diamond needs you, square over
/// budget, dash offline, ring done recently or idle.
class StatusMark extends StatefulWidget {
  const StatusMark({super.key, required this.status, this.size = 10});

  final AgentStatus status;
  final double size;

  @override
  State<StatusMark> createState() => _StatusMarkState();
}

class _StatusMarkState extends State<StatusMark>
    with SingleTickerProviderStateMixin {
  late final _pulse = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1200),
  );

  @override
  void dispose() {
    _pulse.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = widget.status;
    final still = MediaQuery.disableAnimationsOf(context);
    final animate = s == AgentStatus.working && !still;
    if (animate && !_pulse.isAnimating) {
      _pulse.repeat(reverse: true);
    } else if (!animate && _pulse.isAnimating) {
      _pulse.stop();
    }
    final color = switch (s) {
      AgentStatus.needsYou => c.warn,
      AgentStatus.working => c.accent,
      AgentStatus.overBudget => c.error,
      AgentStatus.doneRecently => c.ok,
      _ => c.muted,
    };
    return Semantics(
      label: s.label,
      child: ExcludeSemantics(
        child: Tooltip(
          message: s.label,
          child: AnimatedBuilder(
            animation: _pulse,
            builder: (context, _) => Opacity(
              opacity: animate ? 0.45 + 0.55 * _pulse.value : 1,
              child: CustomPaint(
                size: Size.square(widget.size),
                painter: _MarkPainter(s, color),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _MarkPainter extends CustomPainter {
  _MarkPainter(this.status, this.color);

  final AgentStatus status;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final fill = Paint()..color = color;
    final line = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5;
    final r = size.shortestSide / 2;
    final c = size.center(Offset.zero);
    switch (status) {
      case AgentStatus.working:
        canvas.drawCircle(c, r * 0.8, fill);
      case AgentStatus.needsYou:
        canvas.drawPath(
          Path()
            ..moveTo(c.dx, c.dy - r)
            ..lineTo(c.dx + r, c.dy)
            ..lineTo(c.dx, c.dy + r)
            ..lineTo(c.dx - r, c.dy)
            ..close(),
          fill,
        );
      case AgentStatus.overBudget:
        canvas.drawRect(
          Rect.fromCenter(center: c, width: r * 1.6, height: r * 1.6),
          fill,
        );
      case AgentStatus.offline || AgentStatus.archived:
        canvas.drawLine(
          Offset(c.dx - r * 0.7, c.dy),
          Offset(c.dx + r * 0.7, c.dy),
          line,
        );
      case AgentStatus.doneRecently:
      case AgentStatus.idle:
        canvas.drawCircle(c, r * 0.7, line);
    }
  }

  @override
  bool shouldRepaint(_MarkPainter old) =>
      old.status != status || old.color != color;
}

/// "1 needs you, 2 working": the fleet at a glance. Clicking it opens the
/// first agent that needs you. Shows nothing when nothing is worth a line.
class FleetStrip extends StatelessWidget {
  const FleetStrip({super.key, required this.store, this.padding});

  final TernStore store;
  final EdgeInsetsGeometry? padding;

  @override
  Widget build(BuildContext context) {
    final fleet = store.fleet;
    if (store.link != Link.online || fleet.text.isEmpty) {
      return const SizedBox.shrink();
    }
    final c = context.colors;
    final target = fleet.firstNeedingYou;
    final lead = target != null
        ? AgentStatus.needsYou
        : fleet.working > 0
        ? AgentStatus.working
        : AgentStatus.overBudget;
    return Padding(
      padding: padding ?? const EdgeInsets.fromLTRB(12, 4, 12, 2),
      child: Semantics(
        button: target != null,
        label: 'Fleet: ${fleet.text}',
        child: ExcludeSemantics(
          child: Material(
            color: Colors.transparent,
            child: InkWell(
              key: const ValueKey('fleet-strip'),
              borderRadius: BorderRadius.circular(6),
              onTap: target == null
                  ? null
                  : () => target.isDefault
                        ? store.openHome()
                        : store.openAgent(target),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                child: Row(
                  children: [
                    StatusMark(status: lead, size: 9),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        fleet.text,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: mono.copyWith(fontSize: 11.5, color: c.muted),
                      ),
                    ),
                    if (target != null)
                      Icon(Icons.chevron_right, size: 14, color: c.muted),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Today's routine spend against the daily caps, as a hairline bar and one
/// line. Hidden without a cap or a known price.
class SpendBar extends StatelessWidget {
  const SpendBar({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final spend = spendToday(store.schedules);
    if (spend == null) return const SizedBox.shrink();
    final c = context.colors;
    final share = (spend.spent / spend.cap).clamp(0.0, 1.0);
    final color = share >= 1
        ? c.error
        : share >= 0.8
        ? c.warn
        : c.accent;
    final text =
        'Routines today ${_usd(spend.spent)} of ${_usd(spend.cap)} cap';
    return Padding(
      key: const ValueKey('spend-bar'),
      padding: const EdgeInsets.fromLTRB(18, 2, 14, 2),
      child: Semantics(
        label: text,
        child: ExcludeSemantics(
          child: Tooltip(
            message: 'Spent by routines with a daily USD limit, since midnight',
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  text,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodySmall
                      ?.copyWith(fontSize: 11),
                ),
                const SizedBox(height: 3),
                ClipRRect(
                  borderRadius: BorderRadius.circular(1.5),
                  child: LinearProgressIndicator(
                    value: share,
                    minHeight: 3,
                    color: color,
                    backgroundColor: c.muted.withValues(alpha: 0.2),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

String _usd(double v) =>
    v < 10 ? '\$${v.toStringAsFixed(2)}' : '\$${v.round()}';
