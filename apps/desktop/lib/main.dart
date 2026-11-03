import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import 'state/store.dart';
import 'theme.dart';
import 'ui/home.dart';

void main() {
  final store = TernStore(TernClient(Endpoint.local()))..connect();
  runApp(TernApp(store: store));
}

class TernApp extends StatelessWidget {
  const TernApp({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'tern',
    debugShowCheckedModeBanner: false,
    theme: ternTheme(TernColors.light, Brightness.light),
    darkTheme: ternTheme(TernColors.dark, Brightness.dark),
    home: Home(store: store),
  );
}
