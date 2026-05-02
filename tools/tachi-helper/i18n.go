package main

import (
	"os"
	"strings"
)

var isZh bool

func init() {
	lang := os.Getenv("LANG")
	isZh = strings.HasPrefix(strings.ToLower(lang), "zh")
}

func T(en, zh string) string {
	if isZh {
		return zh
	}
	return en
}
