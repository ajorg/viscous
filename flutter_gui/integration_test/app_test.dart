// Exercises the app with the real native bridge library loaded, so it needs
// a full `flutter build`/`flutter run`-capable toolchain for the target
// platform (unlike the plain widget/golden tests under test/, which never
// touch the bridge and so need none of that). Run with `flutter test
// integration_test` once that toolchain is available.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:viscous_gui/main.dart';
import 'package:viscous_gui/src/rust/frb_generated.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => RustLib.init());

  testWidgets('shows the connect form on launch', (tester) async {
    await tester.pumpWidget(const ViscousApp());
    await tester.pumpAndSettle();

    expect(find.text('Connect'), findsOneWidget);
    expect(find.widgetWithText(TextFormField, 'Serial port'), findsOneWidget);
  });
}
