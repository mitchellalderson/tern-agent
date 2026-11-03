// The secrets dialog against a fake client. Also renders it to
// build/screenshots/secrets_{light,dark}.png (text is boxes without the
// fonts screenshot_test.dart loads).
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/secrets.dart';

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final file = <String, String>{'OPENROUTER_API_KEY': 'or-1'};

  @override
  Future<List<SecretStatus>> secrets() async => [
    SecretStatus(
      name: 'ANTHROPIC_API_KEY',
      inFile: file.containsKey('ANTHROPIC_API_KEY'),
      inEnv: false,
      usedBy: const ['provider anthropic'],
    ),
    const SecretStatus(
      name: 'GITHUB_TOKEN',
      inFile: false,
      inEnv: true,
      usedBy: ['mcp github'],
    ),
    if (file.containsKey('OPENROUTER_API_KEY'))
      const SecretStatus(
        name: 'OPENROUTER_API_KEY',
        inFile: true,
        inEnv: false,
        usedBy: ['provider openrouter'],
      ),
    const SecretStatus(
      name: 'TELEGRAM_BOT_TOKEN',
      inFile: false,
      inEnv: false,
      usedBy: ['telegram'],
    ),
  ];

  @override
  Future<SecretStatus> setSecret(String name, String value) async {
    file[name] = value;
    return (await secrets()).firstWhere((s) => s.name == name);
  }
}

void main() {
  for (final dark in [false, true]) {
    testWidgets('secrets dialog ${dark ? 'dark' : 'light'}', (tester) async {
      tester.view.physicalSize = const Size(760, 560) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      final c = dark ? TernColors.dark : TernColors.light;
      final client = _FakeClient();
      final key = GlobalKey();
      await tester.pumpWidget(
        RepaintBoundary(
          key: key,
          child: MaterialApp(
            debugShowCheckedModeBanner: false,
            theme: ternTheme(c, dark ? Brightness.dark : Brightness.light),
            home: Scaffold(body: SecretsDialog(client: client)),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('Missing'), findsNWidgets(2));
      expect(find.text('From environment'), findsOneWidget);

      // Set the missing Anthropic key.
      await tester.tap(find.widgetWithText(TextButton, 'Set').first);
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField), 'sk-ant-secret');
      await tester.pump();
      final field = tester.widget<TextField>(find.byType(TextField));
      expect(field.obscureText, isTrue);

      await tester.runAsync(() async {
        final boundary =
            key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final image = await boundary.toImage(pixelRatio: 2);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        final out = File(
          'build/screenshots/secrets_${dark ? 'dark' : 'light'}.png',
        );
        await out.create(recursive: true);
        await out.writeAsBytes(png!.buffer.asUint8List());
      });

      await tester.tap(find.widgetWithText(FilledButton, 'Save'));
      await tester.pumpAndSettle();
      expect(client.file['ANTHROPIC_API_KEY'], 'sk-ant-secret');
      expect(find.text('Missing'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
    });
  }
}
