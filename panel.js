function panel(context) {
  const label = context.locale === "zh-CN"
    ? {en: "Video or audio", zh: "视频或音频格式"}
    : {en: "Video or audio"};
  return {fields: [{key: "quality", label}, {key: "playlist"}]};
}
