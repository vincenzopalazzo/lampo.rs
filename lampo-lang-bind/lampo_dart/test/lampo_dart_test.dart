import 'dart:io';

import 'package:test/test.dart';

import 'package:lampo_dart/lampo_dart.dart';

void main() {
  group('Run the lampo node from dart', () {

      test("init lampo node and call the `getinfo`", () async {
          var lampo = Lampo(homePath: "/home/vincent/.lampo/testnet/");
          assert(false);
          var info = await lampo.call(method: "getinfo");
          print(info);
          lampo.stop();
      });

  });
}
