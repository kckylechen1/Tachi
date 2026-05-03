package main

import (
	"fmt"
	"os"

	tea "github.com/charmbracelet/bubbletea"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "version", "-v", "--version":
			fmt.Println("tachi-helper v0.1.0")
			return
		case "help", "-h", "--help":
			fmt.Println("Tachi Setup Wizard — interactive setup for Tachi memory + vault")
			fmt.Println()
			fmt.Println("Usage: tachi-helper")
			fmt.Println()
			fmt.Println("Runs an interactive wizard that helps you:")
			fmt.Println("  • Check Tachi installation and health")
			fmt.Println("  • Initialize / unlock the encrypted Vault")
			fmt.Println("  • Choose API providers and endpoints")
			fmt.Println("  • Import API keys from .env files into Vault")
			fmt.Println("  • Register MCP servers (Exa, Tavily, Context7, etc.)")
			fmt.Println("  • Add shell integration (eval \"$(tachi env)\")")
			return
		}
	}

	p := tea.NewProgram(newWizard(), tea.WithAltScreen())
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}
