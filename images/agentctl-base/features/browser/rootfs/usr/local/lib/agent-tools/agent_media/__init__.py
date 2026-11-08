"""The `media` command, which checks a recording as someone watching it would see it.

A viewer can only read what a clip holds still long enough to read, so that is what it reports, and the frames it
shows are the ones from those times. ffmpeg does the video work, in `video`. What counts as still and which frames to
show are decided in `still`, on plain numbers, so that they can be tested without ffmpeg.
"""
