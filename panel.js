function panel(context) {
  const label = context.locale === "zh-CN"
    ? {en: "Video or audio", zh: "视频或音频格式"}
    : {en: "Video or audio"};
  return {fields: [
    {key: "quality", label},
    {key: "video-container"},
    {key: "video-codec"},
    {key: "playlist"},
    {key: "subtitles"},
    {key: "subtitle-languages"},
    {key: "user-agent"},
    {key: "accept-language"},
  ]};
}
