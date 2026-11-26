# OpenWrt LuCI app: luci-app-ctyun-dashboard
# 放到 openwrt/feeds/luci/applications/ 下，或作为独立 package 编译

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-ctyun-dashboard
PKG_VERSION:=1.0.0
PKG_RELEASE:=1
PKG_LICENSE:=MIT
PKG_MAINTAINER:=likz0421 <hanziwawa1@gmail.com>

LUCI_TITLE:=天翼云电脑控制中心 LuCI 界面
LUCI_DEPENDS:=+curl
LUCI_PKGARCH:=all

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
$(eval $(call BuildPackage,$(PKG_NAME)))
