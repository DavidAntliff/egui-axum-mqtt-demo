#!/bin/bash
convert ui.png -crop 200x50+5+50 connected.png
convert ui.png -crop 700x100+10+120 real-time-send.png
convert ui.png -crop 425x200+10+250 user-initiated-poll.png
convert ui.png -crop 590x100+10+475 real-time-receive.png
convert ui.png -crop 740x130+10+600 ping-device.png

