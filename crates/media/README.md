# Media input

Native media opens through FilmCraft's public random-access reader API. The pinned
source API returns display-oriented video metadata and frames, and normalized AAC
channel order. EffectCraft uses those results directly: it must not apply a second
track rotation or AAC channel remap. This also avoids a redundant file open, movie
header scan, and full-frame rotation buffer.

Generated-media regression tests exercise rotated MP4 dimensions and pixels at full
and reduced resolution, plus per-channel surround-audio gains. These checks use public
source APIs and FFmpeg as an external fixture generator/oracle. FFmpeg is never linked
or shipped. Codec implementations remain in the pinned FilmCraft dependencies.
