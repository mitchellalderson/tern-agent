import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';

import '../state/transcript.dart';
import '../theme.dart';

/// How many entries of a long history a feed shows at first, and how many
/// more each "Show earlier" adds. Older entries are not built, not
/// grouped, not even sliced: only the window is.
const feedWindow = 100;
const feedChunk = 100;

/// How far (in pixels) from the newest entry a reader must be for the feed
/// to hold new arrivals back instead of moving what they are reading.
const _holdOffset = 240.0;

/// Gap kept above the top of a message the feed has anchored.
const _anchorMargin = 16.0;

/// The feed's scroll position. The list is reversed (offset 0 is the
/// bottom), so a growing newest message pushes everything older upward.
/// This position corrects for that during layout, so there is no jitter:
///
/// * pinned to the bottom: follow it, until the newest message would no
///   longer fit, then anchor its top in view and stop following;
/// * scrolled up, or the reader moved: keep what they read in place.
class FeedScrollPosition extends ScrollPositionWithSingleContext {
  FeedScrollPosition({
    required super.physics,
    required super.context,
    super.initialPixels,
    super.oldPosition,
    super.debugLabel,
  });

  /// Height of the newest entry if it is built, else null.
  double? Function()? newestHeight;

  /// Space below the newest entry.
  double bottomPad = 0;

  var _anchored = false;

  /// The anchor may be applied only until the reader first moves; after that
  /// it is released for good (until the next message arrives).
  var _canAnchor = true;
  double? _lastH;
  double _lastPixels = 0;
  var _skip = false;

  /// True while the newest message holds its top in view.
  bool get anchored => _anchored;

  /// A new newest entry arrived: its whole height is growth.
  void arrived() {
    _lastH = 0;
    // A reader parked on the top of a long message stays there.
    _anchored = false;
    _canAnchor = true;
  }

  /// The next height change is the reader's own (a fold opening) and is
  /// handled by them.
  void skipNextGrowth() => _skip = true;

  @override
  bool applyContentDimensions(double minScrollExtent, double maxScrollExtent) {
    final moved =
        hasPixels &&
        hasViewportDimension &&
        _settle(minScrollExtent, maxScrollExtent);
    final settled = super.applyContentDimensions(
      minScrollExtent,
      maxScrollExtent,
    );
    // Laid out at the old offset: do it again at the corrected one.
    return settled && !moved;
  }

  /// Corrects the offset for growth of the newest entry; true if it moved.
  bool _settle(double min, double max) {
    if (pixels != _lastPixels) {
      // The reader (or "latest") moved us: follow the bottom only if they
      // are at it, and never anchor again.
      _anchored = false;
      _canAnchor = false;
    }
    final h = newestHeight?.call();
    if (h == null) {
      _lastPixels = pixels;
      return false;
    }
    final last = _lastH;
    _lastH = h;
    if (last == null) {
      _lastPixels = pixels;
      return false;
    }
    final grew = h - last;
    final skip = _skip;
    _skip = false;
    final atBottom = pixels <= 40;
    final wasFollowing = _anchored || (atBottom && !skip && _canAnchor);
    double? target;
    if (skip || (!wasFollowing && grew == 0)) {
      target = null;
    } else if (wasFollowing) {
      // Is the newest message's top still in view when we sit at the bottom?
      final fit = h + bottomPad + _anchorMargin - viewportDimension;
      if (fit > 0) {
        _anchored = true;
        target = fit;
      } else {
        _anchored = false;
      }
    } else if (pixels <= 0.5 && !skip) {
      // At the bottom after taking over (or "Jump to latest"): follow the
      // bottom, never re-anchor.
      target = 0;
    } else {
      target = pixels + grew;
    }
    var moved = false;
    if (target != null) {
      final t = target.clamp(min, max < min ? min : max);
      if ((t - pixels).abs() > 0.5) {
        correctBy(t - pixels);
        moved = true;
      }
    }
    _lastPixels = pixels;
    return moved;
  }
}

class _FeedController extends ScrollController {
  double? Function()? newestHeight;
  double bottomPad = 0;

  @override
  ScrollPosition createScrollPosition(
    ScrollPhysics physics,
    ScrollContext context,
    ScrollPosition? oldPosition,
  ) =>
      FeedScrollPosition(
          physics: physics,
          context: context,
          initialPixels: initialScrollOffset,
          oldPosition: oldPosition,
          debugLabel: debugLabel,
        )
        ..newestHeight = newestHeight
        ..bottomPad = bottomPad;
}

/// Tell the feed this scrollable belongs to that the next height change of
/// the entry holding [context] is handled by the caller.
void feedSkipNextGrowth(BuildContext context) {
  final pos = Scrollable.maybeOf(context)?.position;
  if (pos is FeedScrollPosition) pos.skipNextGrowth();
}

/// A key that follows an item through rebuilds (a grouped approval is
/// rebuilt each time, so it takes its first member's).
Key itemKey(Item i) => ObjectKey(switch (i) {
  ApprovalGroupItem() => i.members.first,
  PendingApprovalGroupItem() => i.members.first,
  _ => i,
});

/// What a feed shows of its history: the newest [visible] entries, and,
/// while the reader is scrolled up, none of those that arrived after they
/// left the bottom (a "new messages" pill counts them).
class FeedWindow extends ChangeNotifier {
  FeedWindow({this.initial = feedWindow}) : visible = initial;

  final int initial;

  /// Entries shown, newest first.
  int visible;

  /// While set, entries past this count (oldest first) are held back.
  int? frozenTotal;

  /// The entries to build out of [all] (oldest first): the window, ending
  /// at the held-back point.
  List<T> slice<T>(List<T> all) {
    final e = end(all.length);
    return e - start(all.length) == all.length
        ? all
        : all.sublist(start(all.length), e);
  }

  int end(int n) => frozenTotal == null ? n : min(frozenTotal!, n);
  int start(int n) => max(0, end(n) - visible);

  /// Entries older than the window.
  int hiddenEarlier(int n) => start(n);

  /// Entries held back because the reader is scrolled up.
  int hiddenNewer(int n) => n - end(n);

  bool get held => frozenTotal != null;

  void showEarlier([int by = feedChunk]) {
    visible += by;
    notifyListeners();
  }

  void hold(int total) {
    if (frozenTotal == total) return;
    frozenTotal = total;
    notifyListeners();
  }

  void release() {
    if (frozenTotal == null) return;
    frozenTotal = null;
    notifyListeners();
  }

  /// A different conversation: back to the newest, window closed.
  void reset() {
    visible = initial;
    frozenTotal = null;
    notifyListeners();
  }
}

/// A long, newest-at-the-bottom list that stays cheap: lazily built,
/// reversed so the newest stays pinned while it streams, with a
/// "Show earlier" control above the window, and a pill that returns to the
/// newest (counting arrivals held back while the reader was scrolled up).
///
/// [count] entries are drawn, newest first (index 0 is the bottom);
/// [total] is how many exist, for counting those held back.
class FeedList extends StatefulWidget {
  const FeedList({
    super.key,
    required this.window,
    required this.count,
    required this.total,
    required this.itemBuilder,
    this.keyFor,
    this.hiddenEarlier = 0,
    this.moreEarlier = false,
    this.loadingEarlier = false,
    this.onShowEarlier,
    this.padding = const EdgeInsets.symmetric(vertical: 20),
    this.noun = 'messages',
  });

  final FeedWindow window;
  final int count;
  final int total;
  final Widget Function(BuildContext context, int i) itemBuilder;

  /// A stable key for entry `i`, so a card's state stays with its entry as
  /// new ones arrive.
  final Key? Function(int i)? keyFor;

  /// Entries older than the window that are already loaded.
  final int hiddenEarlier;

  /// More exist on the daemon, beyond what is loaded.
  final bool moreEarlier;
  final bool loadingEarlier;

  /// Widen the window, or load an older page. Null: no control.
  final VoidCallback? onShowEarlier;
  final EdgeInsets padding;

  /// What the pill calls new entries.
  final String noun;

  @override
  State<FeedList> createState() => _FeedListState();
}

class _FeedListState extends State<FeedList> {
  late final _scroll = _FeedController()
    ..newestHeight = (() => _newestH)
    ..bottomPad = widget.padding.bottom;
  final _listKey = GlobalKey();
  var _far = false;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_moved);
    _seen = _newest(widget);
  }

  Object? _seen;

  Object? _newest(FeedList w) =>
      w.count == 0 ? null : (w.keyFor?.call(0) ?? w.total);

  @override
  void didUpdateWidget(FeedList old) {
    super.didUpdateWidget(old);
    // Compared with what was seen last, not with `old`, whose callbacks may
    // read a list that has since changed under them.
    final was = _seen;
    _seen = _newest(widget);
    if (was != null && _seen != was && _scroll.hasClients) {
      _newestH = null;
      (_scroll.position as FeedScrollPosition).arrived();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _moved();
      });
    }
  }

  /// Height of the newest entry, reported by its own layout; null until
  /// the one that just arrived has been laid out.
  double? _newestH;

  @override
  void dispose() {
    _scroll
      ..removeListener(_moved)
      ..dispose();
    super.dispose();
  }

  void _moved() {
    if (!_scroll.hasClients) return;
    final far = _scroll.offset > _holdOffset;
    if (far && !widget.window.held) {
      widget.window.hold(widget.total);
    } else if (_scroll.offset < 40 && widget.window.held) {
      widget.window.release();
    }
    if (far != _far) setState(() => _far = far);
  }

  void _latest() {
    widget.window.release();
    if (!_scroll.hasClients) return;
    _scroll.animateTo(
      0,
      duration: const Duration(milliseconds: 220),
      curve: Curves.easeOut,
    );
  }

  @override
  Widget build(BuildContext context) {
    final w = widget;
    final earlier =
        w.onShowEarlier != null && (w.hiddenEarlier > 0 || w.moreEarlier);
    final newer = w.window.hiddenNewer(w.total);
    return Stack(
      children: [
        Positioned.fill(
          child: ListView.builder(
            key: _listKey,
            controller: _scroll,
            reverse: true,
            padding: w.padding,
            itemCount: w.count + (earlier ? 1 : 0),
            findChildIndexCallback: w.keyFor == null
                ? null
                : (key) {
                    for (var i = 0; i < w.count; i++) {
                      if (w.keyFor!(i) == key) return i;
                    }
                    return null;
                  },
            itemBuilder: (context, i) {
              if (i == w.count) {
                return _ShowEarlier(
                  hidden: w.hiddenEarlier,
                  more: w.moreEarlier,
                  loading: w.loadingEarlier,
                  onPressed: w.onShowEarlier!,
                );
              }
              final child = w.itemBuilder(context, i);
              return _Measure(
                key: w.keyFor?.call(i),
                newest: i == 0,
                onHeight: (h) => _newestH = h,
                child: child,
              );
            },
          ),
        ),
        if (_far || newer > 0)
          Positioned(
            left: 0,
            right: 0,
            bottom: 10,
            child: Center(
              child: _LatestPill(
                newer: newer,
                noun: w.noun,
                onPressed: _latest,
              ),
            ),
          ),
      ],
    );
  }
}

class _ShowEarlier extends StatelessWidget {
  const _ShowEarlier({
    required this.hidden,
    required this.more,
    required this.loading,
    required this.onPressed,
  });

  final int hidden;
  final bool more;
  final bool loading;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final label = hidden > 0
        ? 'Show earlier (${more ? '$hidden+' : hidden} more)'
        : 'Show earlier';
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Semantics(
          button: true,
          label: hidden > 0
              ? 'Show earlier messages, $hidden more'
              : 'Show earlier messages',
          excludeSemantics: true,
          child: TextButton(
            onPressed: loading ? null : onPressed,
            child: Text(loading ? 'Loading…' : label),
          ),
        ),
      ),
    );
  }
}

class _LatestPill extends StatelessWidget {
  const _LatestPill({
    required this.newer,
    required this.noun,
    required this.onPressed,
  });

  final int newer;
  final String noun;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final label = newer > 0 ? 'New $noun' : 'Jump to latest';
    return Semantics(
      button: true,
      liveRegion: newer > 0,
      label: newer > 0 ? 'New $noun, jump to latest' : 'Jump to latest',
      excludeSemantics: true,
      child: Material(
        color: c.card,
        elevation: 2,
        shape: StadiumBorder(side: BorderSide(color: c.hairline)),
        child: InkWell(
          customBorder: const StadiumBorder(),
          onTap: onPressed,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 7),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  label,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: FontWeight.w600,
                    color: c.text,
                  ),
                ),
                const SizedBox(width: 4),
                Icon(Icons.arrow_downward, size: 14, color: c.text),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Reports its height as it lays out, while it is the newest entry. An
/// entry's size may only be read by its own render object.
class _Measure extends SingleChildRenderObjectWidget {
  const _Measure({
    super.key,
    required this.newest,
    required this.onHeight,
    required super.child,
  });

  final bool newest;
  final ValueChanged<double> onHeight;

  @override
  RenderObject createRenderObject(BuildContext context) =>
      _RenderMeasure(newest, onHeight);

  @override
  void updateRenderObject(BuildContext context, _RenderMeasure r) => r
    ..newest = newest
    ..onHeight = onHeight;
}

class _RenderMeasure extends RenderProxyBox {
  _RenderMeasure(this.newest, this.onHeight);

  bool newest;
  ValueChanged<double> onHeight;

  @override
  void performLayout() {
    super.performLayout();
    if (newest) onHeight(size.height);
  }
}
