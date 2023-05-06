import 'dart:io';

import 'package:test/test.dart';
import 'package:logger/logger.dart';

import 'package:lampo_dart/lampo_dart.dart';

void main() {
  group('Run the lampo node from dart', () {
      var logger = Logger();

      test("init lampo node and call the `getinfo`", () async {
          var lampo = Lampo(homePath: "/home/vincent/.lampo/testnet/");
          lampo.spawn();
          // Wait that the node wills start
          sleep(Duration(seconds: 2));
          var info = await lampo.call(method: "getinfo");
          logger.d(info);
          lampo.stop();
      });

  });
}
