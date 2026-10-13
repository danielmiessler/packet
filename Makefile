# Packet build. `make install` tests, builds, bundles, signs and installs to /Applications, then relaunches.
APP      := Packet.app
DEST     := /Applications/$(APP)
# An Apple Development identity when the keychain has one; otherwise ad-hoc ("-"), which runs on the Mac that built it.
SIGN_ID  ?= $(or $(shell security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $$2; exit}'),-)
VERSION  := $(shell awk -F'"' '/^version/{print $$2; exit}' Cargo.toml)
ICON_SVG := bundle/AppIcon.svg
GLYPH    := bundle/MenuGlyph.svg
ICONSET  := target/AppIcon.iconset
DMG      := target/Packet-$(VERSION).dmg
# Bundles are staged outside the repo: a checkout under iCloud Drive or another file provider gets app bundles re-tagged
# with FinderInfo within a second, and codesign rejects that as detritus.
STAGE    ?= $(HOME)/Library/Caches/packet-build

.PHONY: build test glyphs icon bundle dmg install uninstall probes clean

build:
	cargo build --release

test:
	cargo test --release --quiet

# The menu-bar glyph in each verdict colour, from the one authored SVG (its only fill is #000000).
# The PNGs are committed so `cargo build` works without librsvg; this target regenerates them.
glyphs: $(GLYPH)
	for p in good:34C759 fair:FF9F0A poor:FF3B30 offline:8E8E93; do \
	  n=$${p%%:*}; c=$${p##*:}; \
	  sed "s/#000000/#$$c/g" $(GLYPH) | rsvg-convert -w 36 -h 36 -o bundle/glyph-$$n.png || exit 1; \
	done

# Every icns size is rendered from the one authored SVG.
icon: target/AppIcon.icns
target/AppIcon.icns: $(ICON_SVG)
	rm -rf $(ICONSET) && mkdir -p $(ICONSET)
	for s in 16 32 128 256 512; do \
	  rsvg-convert -w $$s -h $$s $(ICON_SVG) -o $(ICONSET)/icon_$${s}x$${s}.png || exit 1; \
	  rsvg-convert -w $$((s*2)) -h $$((s*2)) $(ICON_SVG) -o $(ICONSET)/icon_$${s}x$${s}@2x.png || exit 1; \
	done
	iconutil -c icns $(ICONSET) -o $@

bundle: build icon
	rm -rf "$(STAGE)/$(APP)"
	mkdir -p "$(STAGE)/$(APP)/Contents/MacOS" "$(STAGE)/$(APP)/Contents/Resources"
	cp target/release/packet "$(STAGE)/$(APP)/Contents/MacOS/packet"
	cp target/AppIcon.icns "$(STAGE)/$(APP)/Contents/Resources/AppIcon.icns"
	sed 's/0\.1\.0/$(VERSION)/g' bundle/Info.plist > "$(STAGE)/$(APP)/Contents/Info.plist"
	@test "$$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$(STAGE)/$(APP)/Contents/Info.plist")" = AppIcon || (echo "Info.plist does not reference AppIcon" && exit 1)
	xattr -cr "$(STAGE)/$(APP)"
	@test "$(SIGN_ID)" != "-" || echo "no Apple Development identity: signing ad-hoc"
	codesign --force --options runtime --timestamp=none --sign "$(SIGN_ID)" "$(STAGE)/$(APP)"
	codesign --verify --strict "$(STAGE)/$(APP)"

# Drag-to-install image: the app beside an Applications link, with the app icon on the volume.
dmg: bundle
	rm -rf "$(STAGE)/dmg" "$(STAGE)/rw.dmg" "$(DMG)"
	mkdir -p "$(STAGE)/dmg"
	cp -R "$(STAGE)/$(APP)" "$(STAGE)/dmg/$(APP)"
	ln -s /Applications "$(STAGE)/dmg/Applications"
	hdiutil create -quiet -volname Packet -srcfolder "$(STAGE)/dmg" -fs HFS+ -format UDRW -ov "$(STAGE)/rw.dmg"
	hdiutil attach -quiet -readwrite -noverify -noautoopen -mountpoint "$(STAGE)/mnt" "$(STAGE)/rw.dmg"
	xattr -cr "$(STAGE)/mnt/$(APP)"
	codesign --verify --strict "$(STAGE)/mnt/$(APP)"
	cp target/AppIcon.icns "$(STAGE)/mnt/.VolumeIcon.icns"
	SetFile -a C "$(STAGE)/mnt"
	sync
	hdiutil detach -quiet "$(STAGE)/mnt"
	hdiutil convert -quiet "$(STAGE)/rw.dmg" -format UDZO -o "$(DMG)"
	rm -f "$(STAGE)/rw.dmg"
	@echo "built $(DMG)"

install: test bundle
	-pkill -x packet; sleep 0.5
	# Update the bundle in place: deleting and recopying it makes Raycast and other app indexers drop it.
	mkdir -p "$(DEST)"
	rsync -a --delete "$(STAGE)/$(APP)/" "$(DEST)/"
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$(DEST)"
	codesign --verify --strict "$(DEST)"
	open "$(DEST)"

uninstall:
	-pkill -x packet
	rm -rf "$(DEST)"

probes:
	scripts/probe-all.sh

clean:
	cargo clean
	rm -rf "$(STAGE)"
