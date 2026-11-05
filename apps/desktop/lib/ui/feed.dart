import 'dart:math';

import 'package:flutter/material.dart';

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
  final _scroll = ScrollController();
  var _far = false;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_moved);
  }

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
              return switch (w.keyFor?.call(i)) {
                final k? => KeyedSubtree(key: k, child: child),
                null => child,
              };
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
