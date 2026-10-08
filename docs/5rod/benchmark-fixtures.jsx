// Original synthetic benchmark scenes. Run in an otherwise empty project with:
// effectcraft-cli script docs/5rod/benchmark-fixtures.jsx
// Output files are relative to the CLI's working directory.
for (var resolution = 0; resolution < 2; resolution++) {
    var factor = resolution + 1;
    app.run("file.newProject", {});
    app.run("comp.new", {name: "Styled 1080p", width: 1920 * factor, height: 1080 * factor, duration: 3});
    app.run("layer.newSolid", {name: "Plate", width: 1500, height: 800, color: "#e74862"});
    app.run("layer.style.add", {style: "outerGlow"});
    app.run("layer.style.add", {style: "dropShadow"});
    app.run("file.saveAs", {path: resolution === 0 ? "styled.ecproj" : "styled-4k.ecproj"});
}

app.run("file.newProject", {});
for (var resolution = 0; resolution < 2; resolution++) {
    var factor = resolution + 1;
    for (var mode = 0; mode < 2; mode++) {
        app.run("comp.new", {name: (factor === 1 ? "1080p" : "4K") + (mode === 0 ? " Original" : " Arbitrary"), width: 1920 * factor, height: 1080 * factor, frameRate: 30, duration: 1});
        app.run("layer.newSolid", {name: "Glow plate", width: 1500 * factor, height: 800 * factor, color: "#f04364"});
        app.run("effect.apply", {effect: "ec.stylize.glow"});
        app.run("prop.set", {path: "effects/#1/threshold", value: 10});
        app.run("prop.set", {path: "effects/#1/radius", value: 40});
        app.run("prop.set", {path: "effects/#1/colors", value: mode === 0 ? 0 : 2});
        if (mode === 1) app.run("prop.set", {path: "effects/#1/arbitraryMap", value: "0,0 0.4,0.65 1,1 | 0,0 0.5,0.25 1,1 | 0,0 0.6,0.8 1,1"});
    }
}
app.run("file.saveAs", {path: "glow.ecproj"});
