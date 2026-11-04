// The context meter in the conversation header.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/widgets.dart';

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: Center(child: child)),
);

Color? _color(WidgetTester tester) => tester
    .widget<CircularProgressIndicator>(find.byType(CircularProgressIndicator))
    .color;

void main() {
  testWidgets('shows the share of the window in use', (tester) async {
    await tester.pumpWidget(
      _app(const ContextMeter(used: 42000, window: 200000, compactAt: 150000)),
    );
    expect(find.text('21%'), findsOneWidget);
    expect(_color(tester), TernColors.light.muted);
    final tip = tester.widget<Tooltip>(find.byType(Tooltip));
    expect(tip.message, contains('42k of 200k tokens'));
    expect(tip.message, contains('summarized at 150k'));
  });

  testWidgets('warns as it nears summarizing', (tester) async {
    await tester.pumpWidget(
      _app(const ContextMeter(used: 140000, window: 200000, compactAt: 150000)),
    );
    expect(find.text('70%'), findsOneWidget);
    expect(_color(tester), TernColors.light.warn);
  });

  testWidgets('a sliver of use still shows', (tester) async {
    await tester.pumpWidget(
      _app(const ContextMeter(used: 300, window: 1000000)),
    );
    expect(find.text('<1%'), findsOneWidget);
  });

  test('formats large windows in millions', () {
    expect(formatTokens(1000000), '1M');
    expect(formatTokens(1500000), '1.5M');
    expect(formatTokens(200000), '200k');
  });

  test('usage reads the context fields, and does without them', () {
    final u = Usage.fromJson({
      'input_tokens': 10,
      'output_tokens': 5,
      'context_tokens': 12,
      'context_window': 200000,
      'compact_at': 150000,
    });
    expect(
      (u.contextTokens, u.contextWindow, u.compactAt),
      (12, 200000, 150000),
    );
    expect(Usage.fromJson({'input_tokens': 1}).contextTokens, isNull);
  });
}
