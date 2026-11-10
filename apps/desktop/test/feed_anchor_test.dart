// A long new message holds its place: the feed anchors its top in view
// instead of jumping to its bottom, and never moves a reader who scrolled up.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/feed.dart';

/// Newest-first rows of [lines] x 20px, so a row's height is known.
class _Harness extends StatefulWidget {
  const _Harness({super.key});
  @override
  State<_Harness> createState() => _HarnessState();
}

class _HarnessState extends State<_Harness> {
  final window = FeedWindow();
  final rows = <int>[]; // line counts, oldest first
  var next = 0;

  void add(int lines) => setState(() => rows.add(lines));
  void grow(int lines) => setState(() => rows[rows.length - 1] = lines);

  @override
  Widget build(BuildContext context) {
    final shown = window.slice(rows);
    final base = window.start(rows.length);
    return FeedList(
      window: window,
      count: shown.length,
      total: rows.length,
      keyFor: (i) => ValueKey(base + shown.length - 1 - i),
      itemBuilder: (context, i) {
        final id = base + shown.length - 1 - i;
        return Column(
          key: ValueKey('row$id'),
          children: [
            for (var l = 0; l < shown[shown.length - 1 - i]; l++)
              SizedBox(height: 20, width: 300, child: Text('r$id l$l')),
          ],
        );
      },
    );
  }
}

const _view = 500.0;

Future<_HarnessState> _open(WidgetTester tester, int rows) async {
  tester.view.physicalSize = const Size(600, _view);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final key = GlobalKey<_HarnessState>();
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(body: _Harness(key: key)),
    ),
  );
  for (var i = 0; i < rows; i++) {
    key.currentState!.add(3);
  }
  await tester.pump();
  await tester.pump();
  return key.currentState!;
}

ScrollPosition _pos(WidgetTester tester) =>
    tester.state<ScrollableState>(find.byType(Scrollable)).position;

double _top(WidgetTester tester, String text) =>
    tester.getTopLeft(find.text(text)).dy;

Future<void> _settle(WidgetTester tester) async {
  await tester.pump();
  await tester.pump();
}

void main() {
  testWidgets('a: pinned, a short message follows the bottom', (tester) async {
    final h = await _open(tester, 30);
    expect(_pos(tester).pixels, 0);
    h.add(4);
    await _settle(tester);
    expect(_pos(tester).pixels, 0);
    expect(find.text('r30 l3'), findsOneWidget);
  });

  testWidgets('b: a tall message arrives with its top in view', (tester) async {
    final h = await _open(tester, 30);
    h.add(60); // 1200px in a 500px view
    await _settle(tester);
    final top = _top(tester, 'r30 l0');
    // ignore: avoid_print
    print(
      'DBG top=$top px=${_pos(tester).pixels} max=${_pos(tester).maxScrollExtent}',
    );
    expect(top, inInclusiveRange(0, 40));
    expect(
      _top(tester, 'r30 l59'),
      greaterThan(_view),
      reason: 'its bottom is below the fold',
    );
    expect(_pos(tester).pixels, greaterThan(0));
  });

  testWidgets('b: following stops once a growing message outgrows the view', (
    tester,
  ) async {
    final h = await _open(tester, 30);
    h.add(2);
    await _settle(tester);
    // Short: still following the bottom.
    for (final n in [5, 10, 15]) {
      h.grow(n);
      await _settle(tester);
      expect(_pos(tester).pixels, 0, reason: '$n lines still fit');
    }
    // Grows past the view: the top stays put from here on.
    h.grow(30);
    await _settle(tester);
    final top = _top(tester, 'r30 l0');
    expect(top, inInclusiveRange(0, 40));
    for (final n in [40, 55, 80]) {
      h.grow(n);
      await _settle(tester);
      expect(_top(tester, 'r30 l0'), top, reason: 'no drift at $n lines');
    }
  });

  testWidgets('c: scrolled up, nothing moves for a new message', (
    tester,
  ) async {
    final h = await _open(tester, 30);
    _pos(tester).jumpTo(100);
    await _settle(tester);
    final before = _top(tester, 'r27 l0');
    h.add(60);
    await _settle(tester);
    expect(_top(tester, 'r27 l0'), before);
    h.add(2);
    await _settle(tester);
    expect(_top(tester, 'r27 l0'), before);
  });

  testWidgets('c: far up, new messages wait behind the pill', (tester) async {
    final h = await _open(tester, 30);
    _pos(tester).jumpTo(600);
    await _settle(tester);
    final before = _pos(tester).pixels;
    h.add(60);
    await _settle(tester);
    expect(_pos(tester).pixels, before);
    expect(find.text('New messages'), findsOneWidget);
  });

  testWidgets('c: a reader who moves off the anchor is left alone', (
    tester,
  ) async {
    final h = await _open(tester, 30);
    h.add(2);
    await _settle(tester);
    h.grow(60);
    await _settle(tester);
    final p = _pos(tester);
    p.jumpTo(p.pixels + 200); // reading on
    await _settle(tester);
    final before = _top(tester, 'r30 l20');
    h.grow(90);
    await _settle(tester);
    expect(_top(tester, 'r30 l20'), before);
  });

  testWidgets('d: a message streaming in does not jitter', (tester) async {
    final h = await _open(tester, 30);
    h.add(1);
    await _settle(tester);
    final tops = <double>[];
    for (var n = 2; n <= 70; n += 3) {
      h.grow(n);
      await tester.pump(); // each frame as it lands, not after a second pass
      if (n * 20 + 20 > _view) tops.add(_top(tester, 'r30 l0'));
    }
    expect(tops, isNotEmpty);
    expect(tops.toSet().length, 1, reason: 'top held still: $tops');
  });
}
