package store_test

import (
	"testing"

	"github.com/pop-dev/popserver/internal/store"
	"github.com/pop-dev/popserver/internal/store/storetest"
)

func TestMemoryStoreConformance(t *testing.T) {
	storetest.Run(t, func(t *testing.T) store.Store {
		return store.NewMemory()
	})
}
