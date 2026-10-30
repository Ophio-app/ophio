# SPDX-License-Identifier: Apache-2.0
"""The phone app's data never leaves the device through backup or migration."""
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
MAIN = ROOT / "client/src-tauri/gen/android/app/src/main"
ANDROID = "{http://schemas.android.com/apk/res/android}"
# Every storage domain Android's backup agent copies, credential- and
# device-protected alike.
DOMAINS = {
    "root", "file", "database", "sharedpref", "external",
    "device_root", "device_file", "device_database", "device_sharedpref",
}


class AndroidBackupTests(unittest.TestCase):
    def test_backup_and_device_transfer_exclude_all_app_data(self):
        application = ET.parse(MAIN / "AndroidManifest.xml").find("application")
        # Android 11 and older honour allowBackup for every kind of copy.
        self.assertEqual(application.get(ANDROID + "allowBackup"), "false")
        # From Android 12, allowBackup no longer stops device-to-device
        # transfer; only extraction rules do.
        rules = application.get(ANDROID + "dataExtractionRules")
        self.assertEqual(rules, "@xml/data_extraction_rules")
        document = ET.parse(MAIN / "res/xml/data_extraction_rules.xml").getroot()
        for section in ("cloud-backup", "device-transfer"):
            with self.subTest(section=section):
                element = document.find(section)
                self.assertIsNotNone(element)
                self.assertEqual(element.findall("include"), [])
                excluded = {
                    e.get("domain")
                    for e in element.findall("exclude")
                    if e.get("path") == "."
                }
                self.assertEqual(excluded, DOMAINS)


if __name__ == "__main__":
    unittest.main()
