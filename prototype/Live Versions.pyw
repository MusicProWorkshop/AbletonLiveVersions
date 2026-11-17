# Double-click to start the hotkey service without a console window.
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from liveversions.__main__ import main  # noqa: E402

main(sys.argv[1:] or ["run"])
